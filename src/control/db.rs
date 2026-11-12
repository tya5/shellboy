//! Durable Control operations. Runtime effects are returned only after commit.
pub(super) mod archive;
mod execute_kind;
mod reparent;
mod run_action;
mod tags;
pub(crate) mod thread_summary;
use crate::digest::sha256_hex as hash;
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    config::Config,
    history,
    protocol::*,
    storage::{DbWorker, StorageError},
};

struct SendContent {
    elements: Vec<Value>,
    sender: MessageSender,
}

use super::receipt::Admission;

type Result<T> = std::result::Result<T, ApiError>;

/// An immutable submission snapshot stored separately from the editable draft.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftSubmission {
    pub draft_id: String,
    pub source_draft_id: String,
    pub revision: u64,
    pub snapshot: Value,
}
impl DraftSubmission {
    pub fn id_for_operation(operation_id: &str) -> String {
        format!("draft-submission:{}", hash(operation_id.as_bytes()))
    }
}
#[derive(Debug, Clone, Default)]
pub struct DraftSendOptions {
    pub deliver: Deliver,
    pub submission: Option<DraftSubmission>,
    pub expected_selection_revision: Option<i64>,
    /// Control's canonical preflight-independent request hash; never a wire field.
    #[doc(hidden)]
    pub request_fingerprint: Option<String>,
}

/// Original human draft request identity, available before filesystem planning.
/// Derived revisions/observations never change whether a saved request replays.
pub(crate) fn draft_send_fingerprint(
    args: &ThreadCreateArgs,
    thread: Option<&str>,
    content: &crate::attachments::DraftContent,
    files: &[crate::attachments::AttachmentInput],
    options: &DraftSendOptions,
) -> Result<String> {
    let files = files
        .iter()
        .map(|file| {
            let path = file.path.to_str().ok_or_else(|| {
                error(
                    ErrorCode::InvalidArgument,
                    "Attachment path must be valid Unicode",
                )
            })?;
            Ok(json!({"path":path,"kind":file.kind}))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut intent = json!({"internal_operation":"draft_send_raw","args":args,"thread_id":thread,"text":content.text,"files":files,"deliver":options.deliver,"submission":options.submission});
    content.fingerprint_positions(&mut intent);
    Ok(hash(serde_json::to_vec(&intent).map_err(|_| {
        error(ErrorCode::InvalidArgument, "Invalid draft request identity")
    })?))
}

#[derive(Debug, Clone, Default)]
pub struct MutationResult {
    /// A committed lifecycle transition that rejects the requested action.
    pub(super) committed_error: Option<ApiError>,
    pub owner_shutdown: bool,
    pub result: Value,
    pub wake_threads: Vec<String>,
    /// Publish committed metadata without starting or resuming a driver.
    pub changed_threads: Vec<String>,
    /// Notify a driver without interrupting its foreground shell/tool group.
    pub quiet_wake_threads: Vec<String>,
    pub stop_runs: Vec<String>,
    pub compact_threads: Vec<String>,
}
impl MutationResult {
    pub(super) fn value(result: impl Serialize) -> Result<Self> {
        Ok(Self {
            result: serde_json::to_value(result)
                .map_err(|_| error(ErrorCode::InvalidState, "cannot serialize operation result"))?,
            ..Self::default()
        })
    }
}

#[derive(Debug)]
pub enum RuntimeReservation {
    New,
    Pending,
    Saved(std::result::Result<Value, ApiError>),
}

/// Result of one receipt-collection pass (storage design #171).
#[derive(Debug, Default, Clone, Copy)]
pub struct ReceiptPass {
    pub collected: u64,
    /// The pass stopped at a deferred unit; resume on the next trigger.
    pub deferred: bool,
    /// Earliest future age eligibility seen, as a SQLite Julian day.
    pub due_julian: Option<f64>,
}

#[derive(Clone)]
pub struct Store {
    pub(crate) clients: Arc<crate::model::http::HttpClients>,
    pub database: DbWorker,
    pub config: Arc<Config>,
    pub index: Arc<crate::index::IndexService>,
    pub startup_cwd: PathBuf,
    pub(crate) instructions: crate::instructions::InstructionLoader,
    pub instance_id: String,
    pub(crate) usage_diagnostics: Option<crate::diagnostics::Diagnostics>,
    usage_context: crate::usage_totals::ReadContext,
    /// Receipt-collection background requester, shared by clones of this Store.
    background: Arc<tokio::sync::Mutex<crate::storage::BackgroundRequester>>,
}

#[derive(Clone)]
struct AgentsPlan {
    command: Command,
    source: Option<(String, i64)>,
}

#[derive(Serialize, Deserialize)]
struct SavedOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ApiError>,
}

/// Reclamation must not replace malformed durable data with an expiry marker.
pub(super) fn valid_saved_outcome(body: &str) -> bool {
    serde_json::from_str::<SavedOutcome>(body).is_ok_and(|saved| {
        matches!(
            (saved.result, saved.error),
            (Some(_), None) | (None, Some(_))
        )
    })
}

impl Store {
    pub fn new(
        database: DbWorker,
        config: Arc<Config>,
        startup_cwd: PathBuf,
        instance_id: String,
    ) -> Self {
        let clients = config.live_connections.clients(&config);
        Self {
            index: crate::index::IndexService::with_clients(
                database.clone(),
                config.clone(),
                instance_id.clone(),
                clients.clone(),
            ),
            clients,
            instructions: crate::instructions::InstructionLoader::new(&config),
            background: Arc::new(tokio::sync::Mutex::new(
                crate::storage::BackgroundRequester::new(database.clone()),
            )),
            database,
            config,
            startup_cwd,
            instance_id,
            usage_diagnostics: None,
            usage_context: crate::usage_totals::ReadContext::default(),
        }
    }

    /// Background maintenance pass over every operation receipt in bounded
    /// rowid windows. A deferred window ends this pass early (`deferred`);
    /// committed windows stay collected. `due` is the earliest future instant
    /// at which a seen terminal result becomes collectable by age: the
    /// requester waits for it instead of repeating on a timer.
    pub async fn collect_receipts(&self) -> Result<ReceiptPass> {
        let mut pass = ReceiptPass::default();
        let mut after = 0;
        loop {
            let instance = self.instance_id.clone();
            // Waiting admissions can interleave between maintenance windows.
            let window = self
                .background
                .lock()
                .await
                .unit(super::receipt::COLLECT_WINDOW, move |db, window| {
                    let tx = db.transaction()?;
                    let result =
                        super::receipt::under_pressure(&tx, &instance).and_then(|pressure| {
                            super::receipt::collect_window(&tx, &instance, pressure, after, window)
                        });
                    if result.is_ok() {
                        tx.commit()?;
                    }
                    Ok(result)
                })
                .await
                .map_err(storage_error)?;
            let crate::storage::Background::Done(window) = window else {
                pass.deferred = true;
                return Ok(pass);
            };
            let window = window?;
            pass.collected += window.collected;
            if let Some(due) = window.due {
                pass.due_julian = Some(pass.due_julian.map_or(due, |old: f64| old.min(due)));
            }
            match window.next {
                Some(next) => after = next,
                None => return Ok(pass),
            }
            tokio::task::yield_now().await;
        }
    }

    async fn receipt_attempt<T, F>(
        &self,
        kind: Option<execute_kind::Kind>,
        requester: Option<&'static str>,
        attempt: F,
    ) -> Result<(Result<T>, bool, u64)>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &mut bool) -> std::result::Result<Result<T>, StorageError>
            + Send
            + 'static,
    {
        let job = move |db: &mut Connection| {
            let mut pressure = false;
            let result = attempt(db, &mut pressure)?;
            Ok((result, pressure, db.total_changes()))
        };
        match kind {
            Some(kind) => {
                execute_kind::call_with_requester(&self.database, kind, requester, job).await
            }
            None => self.database.call_with_requester(requester, job).await,
        }
        .map_err(storage_error)
    }

    /// Retry admission outside the writer between bounded background windows.
    /// Concurrent writes invalidate an exhausted scan, so refusal is based on
    /// a complete pass with no intervening changes outside our own windows.
    async fn receipt_job<T, F>(
        &self,
        kind: Option<execute_kind::Kind>,
        origin_kind: Option<&'static str>,
        operation: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &mut bool) -> std::result::Result<Result<T>, StorageError>
            + Clone
            + Send
            + 'static,
    {
        let mut after = Some(0i64);
        // All writes share this connection; total_changes also counts triggers
        // and conservatively detects changes even if their transaction rolls back.
        let mut observed = None;
        let mut dirty = false;
        loop {
            let attempt = operation.clone();
            let (result, pressure, changes) =
                self.receipt_attempt(kind, origin_kind, attempt).await?;
            if !pressure {
                return result;
            }
            if observed.is_some_and(|previous| previous != changes) {
                dirty = true;
            }
            if after.is_none() {
                if !dirty {
                    return result;
                }
                after = Some(0);
                dirty = false;
            }
            observed = Some(changes);
            loop {
                let mut requester = self.background.lock().await;
                let current = self
                    .database
                    .call(|db| Ok(db.total_changes()))
                    .await
                    .map_err(storage_error)?;
                if observed.is_some_and(|previous| previous != current) {
                    // Another unit may have freed enough room while this
                    // requester waited for the background lane.
                    dirty = true;
                    let attempt = operation.clone();
                    let (result, pressure, changes) =
                        self.receipt_attempt(kind, origin_kind, attempt).await?;
                    if !pressure {
                        return result;
                    }
                    observed = Some(changes);
                }
                let instance = self.instance_id.clone();
                let cursor = after.expect("unfinished pass");
                let unit = requester
                    .unit(super::receipt::COLLECT_WINDOW, move |db, window| {
                        let before = db.total_changes();
                        let result = {
                            let tx = db.transaction()?;
                            match super::receipt::collect_window(
                                &tx, &instance, true, cursor, window,
                            ) {
                                Ok(window) => {
                                    tx.commit()?;
                                    Ok(window)
                                }
                                Err(error) => Err(error),
                            }
                        };
                        Ok(result.map(|window| (window, before, db.total_changes())))
                    })
                    .await
                    .map_err(storage_error)?;
                drop(requester);
                match unit {
                    crate::storage::Background::Done(window) => {
                        let (window, before, after_changes) = window?;
                        if observed.is_some_and(|previous| previous != before) {
                            dirty = true;
                        }
                        observed = Some(after_changes);
                        after = window.next;
                        break;
                    }
                    crate::storage::Background::Deferred => tokio::task::yield_now().await,
                }
            }
        }
    }

    /// `caller` has already been authenticated against the server's origin map.
    /// A client-supplied Request.origin is never independently trusted here.
    pub async fn execute(
        &self,
        request: Request,
        caller: Option<Origin>,
    ) -> Result<MutationResult> {
        request.validate()?;
        if matches!(&request.command, Command::Status(a) if a.memory) {
            return Err(error(
                ErrorCode::InvalidArgument,
                "Owner memory requires the runtime observation route",
            ));
        }
        if let Command::Status(mut args) = request.command.clone() {
            if args.thread_id.is_none() && args.run_id.is_none() {
                args.thread_id = caller.as_ref().map(|origin| origin.thread_id.clone());
            }
            let instance = self.instance_id.clone();
            let path = self.database.path().to_path_buf();
            let observer = self.database.clone();
            let kind = format!("{:?}", execute_kind::Kind::of(&request.command));
            let queued_at = std::time::Instant::now();
            let requester = Some(if caller.is_some() {
                "shell"
            } else {
                "external"
            });
            return inspection_work(move || {
                let mut db = open_inspection(&path)?;
                observer.observe_inspection(&mut db, &kind, queued_at, requester, |db| {
                    let tx = db.transaction().map_err(sql_error)?;
                    let result = crate::status::snapshot(&tx, &args, &instance)?;
                    tx.commit().map_err(sql_error)?;
                    MutationResult::value(result)
                })
            })
            .await?;
        }
        if let Command::CompactPreview(args) = &request.command {
            let thread = target(args.thread_id.as_deref(), caller.as_ref())?;
            let args = args.clone();
            let config = self.config.clone();
            let path = self.database.path().to_path_buf();
            let observer = self.database.clone();
            let kind = format!("{:?}", execute_kind::Kind::of(&request.command));
            let queued_at = std::time::Instant::now();
            let requester = Some(if caller.is_some() {
                "shell"
            } else {
                "external"
            });
            return inspection_work(move || {
                let mut db = open_inspection(&path)?;
                let raw =
                    observer.observe_inspection(&mut db, &kind, queued_at, requester, |db| {
                        let tx = db.transaction().map_err(sql_error)?;
                        let raw = crate::compaction::preview::read_tx(&tx, &thread, &args, &config)
                            .map_err(storage_error)?;
                        tx.commit().map_err(sql_error)?;
                        Ok::<_, ApiError>(raw)
                    })?;
                drop(db);
                MutationResult::value(crate::compaction::preview::prepare(raw))
            })
            .await?;
        }
        if let Command::AttachmentList(args) = &request.command {
            let thread = target(args.thread_id.as_deref(), caller.as_ref())?;
            let path = self.database.path().to_path_buf();
            let observer = self.database.clone();
            let kind = format!("{:?}", execute_kind::Kind::of(&request.command));
            let queued_at = std::time::Instant::now();
            let requester = Some(if caller.is_some() {
                "shell"
            } else {
                "external"
            });
            let mut items = inspection_work(move || {
                let mut db = open_inspection(&path)?;
                observer.observe_inspection(&mut db, &kind, queued_at, requester, |db| {
                    let tx = db.transaction().map_err(sql_error)?;
                    let items = crate::attachments::list_tx(&tx, &thread)?;
                    tx.commit().map_err(sql_error)?;
                    Ok::<_, ApiError>(items)
                })
            })
            .await??;
            return MutationResult::value(
                self.instructions
                    .file_work(move || {
                        crate::attachments::observe_paths(&mut items);
                        Ok(items)
                    })
                    .await?,
            );
        }
        if execute_kind::Kind::of(&request.command).read_lane() {
            let mut store = self.clone();
            store.usage_context = crate::usage_totals::read_context();
            let path = self.database.path().to_path_buf();
            let observer = self.database.clone();
            let kind = format!("{:?}", execute_kind::Kind::of(&request.command));
            let queued_at = std::time::Instant::now();
            let requester = Some(if caller.is_some() {
                "shell"
            } else {
                "external"
            });
            return inspection_work(move || {
                let mut db = open_inspection(&path)?;
                observer.observe_inspection(&mut db, &kind, queued_at, requester, |db| {
                    let tx = db.transaction().map_err(sql_error)?;
                    let value = store.dispatch(
                        &tx,
                        &request.command,
                        caller.as_ref(),
                        &request.operation_id,
                        &chrono::Utc::now().to_rfc3339(),
                    );
                    tx.commit().map_err(sql_error)?;
                    value
                })
            })
            .await?;
        }
        let plan = Arc::new(self.prepare_agents(&request, &caller).await?);
        let request = Arc::new(request);
        let fingerprint = Arc::new(std::sync::OnceLock::new());
        let mut store = self.clone();
        store.usage_context = crate::usage_totals::read_context();
        let kind = execute_kind::Kind::of(&request.command);
        if !kind.writer_allowed(&request.command) {
            return Err(error(
                ErrorCode::InvalidState,
                "Command is not admitted to the writer lane",
            ));
        }
        let requester = Some(if caller.is_some() {
            "shell"
        } else {
            "external"
        });
        self.receipt_job(Some(kind), requester, move |connection, pressure| {
            Ok(store.execute_transaction(
                connection,
                &request,
                caller.as_ref(),
                plan.as_ref().as_ref(),
                &fingerprint,
                pressure,
            ))
        })
        .await
    }

    pub async fn execute_with_plan(
        &self,
        request: Request,
        caller: Option<Origin>,
        plan: crate::attachments::AddPlan,
    ) -> Result<MutationResult> {
        request.validate()?;
        let request = Arc::new(request);
        let plan = Arc::new(plan);
        let fingerprint = Arc::new(std::sync::OnceLock::new());
        let store = self.clone();
        let requester = Some(if caller.is_some() {
            "shell"
        } else {
            "external"
        });
        self.receipt_job(None, requester, move |db, pressure| {
                let fingerprint = match request_fingerprint(&fingerprint, &request, caller.as_ref()) {
                    Ok(fingerprint) => fingerprint,
                    Err(error) => return Ok(Err(error)),
                };
                Ok(store.transact(
                    db,
                    &request.operation_id,
                    fingerprint,
                    caller.as_ref(),
                    Admission::Normal,
                    pressure,
                    |tx, now| {
                        let target_id = target(
                            match &request.command {
                                Command::AttachmentAdd(a) => a.thread_id.as_deref(),
                                _ => None,
                            },
                            caller.as_ref(),
                        )?;
                        if target_id != plan.thread_id {
                            return Err(error(
                                ErrorCode::InvalidArgument,
                                "Attachment target changed",
                            ));
                        }
                        let current:Option<i64>=history::selection_revision(tx, &target_id, history::ThreadScope::Live).optional().map_err(sql_error)?;
                        if current.is_none(){return Err(error(ErrorCode::ThreadNotFound,"Attachment target was deleted before acceptance"));}
                        if current!=Some(plan.selection_revision){return Err(error(ErrorCode::InvalidState,"Attachment selection changed; retry from current Thread state"));}
                        if let Some(origin)=caller.as_ref() {
                            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution s JOIN tool_call c ON c.operation_id=s.id JOIN tool_group g ON g.id=c.group_id WHERE s.id=?1 AND s.owner_run_id=?2 AND s.turn_id=?3)",params![origin.execution_id,origin.run_id,origin.turn_id],|r|r.get(0)).map_err(sql_error)?;
                            if !valid { return Err(error(ErrorCode::InvalidState,"Attachment origin has no durable shell tool group")); }
                        }
                        let source=uuid::Uuid::new_v4().to_string();
                        let origin=caller.as_ref().map(|origin|json!({"kind":"shell","thread_id":origin.thread_id,"run_id":origin.run_id,"turn_id":origin.turn_id,"execution_id":origin.execution_id})).unwrap_or_else(||json!({"kind":"external_cli"}));
                        let target_run:Option<String>=tx.query_row("SELECT id FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",[&target_id],|r|r.get(0)).optional().map_err(sql_error)?;
                        let held:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM run r LEFT JOIN turn u ON u.run_id=r.id WHERE r.thread_id=?1 AND (r.state='stopping' OR (r.state='live' AND u.state='recovery_wait')))",[&target_id],|r|r.get(0)).map_err(sql_error)?;
                        let content=crate::attachments::DraftContent::appended(format!("Shellboy attachment addition v1: {}. This is a Shellboy auxiliary attachment operation, not a new human utterance.",origin),plan.files.len()).new_source_elements(plan.files.len())?;
                        let payload=json!({"role":"user","content":content,"attachment_operation":{"operation_id":request.operation_id,"origin":origin}});
                        let body=payload.to_string();
                        tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,'event',?2,?3,?4,?5)",params![source,now,body,body.len() as i64,hash(body.as_bytes())]).map_err(sql_error)?;
                        tx.execute("INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at,target_run_id)VALUES(?1,?2,?3,?4,?5,?6,?7)",params![source,target_id,if held {"held"} else {"accepted"},request.operation_id,origin.to_string(),now,target_run]).map_err(sql_error)?;
                        crate::attachments::add_tx(tx,&plan,Some(&source))?;
                        bump_roots(tx)?;
                        store.index.authorize_new_tx(tx,std::slice::from_ref(&source))?;
                        let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM run WHERE thread_id=?1 AND state IN ('live','stopping'))",[&target_id],|r|r.get(0)).map_err(sql_error)?;
                        if !live && !held { super::attachment_delivery::apply_pending_tx(tx,&target_id,now)?; }
                        let attached=crate::attachments::list_tx(tx,&target_id)?.into_iter().filter(|a|a.source_node_id==source).collect::<Vec<_>>();
                        let mut result=MutationResult::value(attached)?;
                        if live && !held { result.quiet_wake_threads.push(target_id); } else { result.changed_threads.push(target_id); }
                        Ok(result)
                    },
                ))
            })
            .await
    }

    /// Draft text and its references are accepted in the same transaction.
    pub async fn send_draft(
        &self,
        operation_id: String,
        args: ThreadCreateArgs,
        thread_id: Option<String>,
        text: String,
        files: Vec<crate::attachments::PlannedAttachment>,
        expected_revision: Option<i64>,
    ) -> Result<MutationResult> {
        let content = crate::attachments::DraftContent::appended(text, files.len());
        self.send_draft_content(
            operation_id,
            args,
            thread_id,
            content,
            files,
            DraftSendOptions {
                expected_selection_revision: expected_revision,
                ..Default::default()
            },
        )
        .await
    }
    pub(crate) async fn replay_draft_send(
        &self,
        operation_id: &str,
        fingerprint: &str,
    ) -> Result<Option<MutationResult>> {
        let operation = operation_id.to_owned();
        let fingerprint = fingerprint.to_owned();
        let active = operation_id
            .split_once(':')
            .is_some_and(|(instance, nonce)| instance == self.instance_id && !nonce.is_empty());
        self.database
            .call(move |db| {
                if let Some(saved) =
                    saved_operation(db, &operation, &fingerprint).map_err(StorageError::from)?
                {
                    return Ok(Some(saved));
                }
                if !active {
                    return Err(StorageError::from(error(
                        ErrorCode::OperationExpired,
                        "Unknown draft operation belongs to an earlier owner",
                    )));
                }
                Ok(None)
            })
            .await
            .map_err(storage_error)
    }
    pub async fn prepare_draft_submission(
        &self,
        operation_id: &str,
        thread_id: Option<&str>,
        submission: &DraftSubmission,
    ) -> Result<()> {
        if !operation_id
            .split_once(':')
            .is_some_and(|(instance, nonce)| instance == self.instance_id && !nonce.is_empty())
        {
            return Err(error(
                ErrorCode::OperationExpired,
                "Draft submission has no active owner operation identity",
            ));
        }
        let operation = operation_id.to_owned();
        let thread = thread_id.map(str::to_owned);
        let submission = submission.clone();
        let config = self.config.clone();
        self.database
            .call(move |db| {
                super::pending_messages::prepare_submission(
                    db,
                    &operation,
                    thread.as_deref(),
                    &submission,
                    &config,
                )
            })
            .await
            .map_err(storage_error)
    }
    pub(crate) async fn send_draft_content(
        &self,
        operation_id: String,
        args: ThreadCreateArgs,
        thread_id: Option<String>,
        content: crate::attachments::DraftContent,
        files: Vec<crate::attachments::PlannedAttachment>,
        options: DraftSendOptions,
    ) -> Result<MutationResult> {
        content.validate(files.len())?;
        if thread_id.is_none() {
            Command::ThreadCreate(args.clone()).validate()?;
        }
        let paths: Vec<Value> = files
            .iter()
            .map(|f| json!({"path":f.path,"kind":f.kind,"managed_id":f.managed_id}))
            .collect();
        let mut intent = json!({"internal_operation":"draft_send","args":args,"thread_id":thread_id,"text":content.text,"files":paths,"deliver":options.deliver,"submission":options.submission});
        content.fingerprint_positions(&mut intent);
        let fingerprint = options
            .request_fingerprint
            .clone()
            .unwrap_or_else(|| hash(serde_json::to_vec(&intent).unwrap()));
        drop(intent);
        let store = self.clone();
        let args = Arc::new(args);
        let options = Arc::new(options);
        let content = Arc::new(content);
        let files = Arc::new(files);
        self.receipt_job(None, None, move |db, pressure| {
            Ok(store.transact(
                db,
                &operation_id,
                &fingerprint,
                None,
                Admission::Normal,
                pressure,
                |tx, now| {
                    let target = if let Some(thread) = thread_id {
                        thread
                    } else {
                        store.create_thread(tx, &args, now)?
                    };
                    let revision: i64 =
                        history::selection_revision(tx, &target, history::ThreadScope::Any)
                            .map_err(sql_error)?;
                    if options
                        .expected_selection_revision
                        .is_some_and(|value| value != revision)
                    {
                        return Err(error(
                            ErrorCode::InvalidState,
                            "Draft attachment selection changed; retry from the retained draft",
                        ));
                    }
                    let result = store.send_content(
                        tx,
                        &MessageSendArgs {
                            human: true,
                            resume_recovery: false,
                            to_human: false,
                            expect_reply: false,
                            sender_label: None,
                            thread_id: Some(target),
                            text: content.text.clone(),
                            deliver: options.deliver,
                        },
                        None,
                        &operation_id,
                        now,
                        SendContent {
                            elements: content.new_source_elements(files.len())?,
                            sender: MessageSender::recorded(None, None, true),
                        },
                    )?;
                    let node = result
                        .result
                        .get("message_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            error(ErrorCode::InvalidState, "Accepted draft has no source node")
                        })?;
                    // The caller's pre-send CAS was checked above. Recovery
                    // may legitimately adopt older input in this same tx;
                    // registration must use the resulting local revision.
                    let registration_revision = history::selection_revision(
                        tx,
                        result.result["thread_id"].as_str().unwrap(),
                        history::ThreadScope::Live,
                    )
                    .map_err(sql_error)?;
                    crate::attachments::add_tx(
                        tx,
                        &crate::attachments::AddPlan {
                            thread_id: result.result["thread_id"].as_str().unwrap().into(),
                            selection_revision: registration_revision,
                            files: files.as_ref().clone(),
                        },
                        Some(node),
                    )?;
                    if let Some(submission) = &options.submission {
                        super::pending_messages::submit_draft_tx(
                            tx,
                            &operation_id,
                            submission,
                            &result.result,
                            now,
                            &store.config,
                        )
                        .map_err(storage_error)?;
                    }
                    Ok(result)
                },
            ))
        })
        .await
    }

    /// Read a committed receipt before mutable filesystem/target preflight.
    pub async fn replay(
        &self,
        request: &Request,
        caller: &Option<Origin>,
    ) -> Result<Option<MutationResult>> {
        let fingerprint = hash(
            serde_json::to_vec(&fingerprint_body(&request.command, caller.as_ref()))
                .map_err(|_| error(ErrorCode::InvalidArgument, "invalid request encoding"))?,
        );
        let operation = request.operation_id.clone();
        self.database
            .call(move |db| Ok(saved_operation(db, &operation, &fingerprint)))
            .await
            .map_err(storage_error)?
    }

    /// Commit intent before an OS-side operation. A pending record is never
    /// treated as permission to execute the same side effect a second time.
    pub async fn reserve_runtime(
        &self,
        request: Request,
        caller: Option<Origin>,
    ) -> Result<RuntimeReservation> {
        request.validate()?;
        let fingerprint = hash(
            serde_json::to_vec(&fingerprint_body(&request.command, caller.as_ref()))
                .map_err(|_| error(ErrorCode::InvalidArgument, "invalid request encoding"))?,
        );
        let preflight = super::receipt::runtime_preflight(&request).err();
        let admission = Admission::for_command(&request.command);
        let stored_key = matches!(request.command, Command::CredentialsSet(_));
        self.reserve_hashed(
            request.operation_id,
            caller,
            fingerprint,
            preflight,
            admission,
        )
        .await
        .map_err(|mut conflict| {
            if stored_key && conflict.code == ErrorCode::OperationConflict {
                // The fingerprint of a stored key is salted per owner process (see
                // `fingerprint_body`), so after an owner restart the same retry cannot match.
                conflict.hint = Some("If this operation ID was used before an owner restart, the saved request cannot be compared with the key you sent now. Read config get (credential_source) to see whether the key is stored, then repeat with a new operation ID".into());
            }
            conflict
        })
    }
    pub async fn reserve_internal(
        &self,
        operation_id: String,
        body: Value,
    ) -> Result<RuntimeReservation> {
        self.reserve_hashed(
            operation_id,
            None,
            hash(serde_json::to_vec(&json!({"internal_operation":body})).unwrap()),
            None,
            Admission::Normal,
        )
        .await
    }
    async fn reserve_hashed(
        &self,
        operation_id: String,
        caller: Option<Origin>,
        fingerprint: String,
        preflight: Option<ApiError>,
        admission: Admission,
    ) -> Result<RuntimeReservation> {
        let instance_id = self.instance_id.clone();
        self.receipt_job(None, None, move|db,pressure| {
            let tx=db.transaction()?;
            let saved:Option<(String,Option<String>)>=tx.query_row("SELECT request_hash,result_ref_json FROM operation_receipt WHERE operation_id=?1",[&operation_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            if let Some((previous,body))=saved {
                if previous!=fingerprint {return Ok(Err(error(ErrorCode::OperationConflict,"operation ID was already used with different arguments")));}
                let result=if let Some(body)=body {
                    if body==super::receipt::EXPIRED_OUTCOME {return Ok(Ok(RuntimeReservation::Saved(Err(super::receipt::expired_error()))));}
                    let outcome:SavedOutcome=serde_json::from_str(&body).map_err(|_|StorageError::Schema("saved runtime operation result is malformed".into()))?;
                    match (outcome.result,outcome.error) {
                        (Some(value),None)=>RuntimeReservation::Saved(Ok(value)),
                        (None,Some(error))=>RuntimeReservation::Saved(Err(error)),
                        _=>return Ok(Err(error(ErrorCode::InvalidState,"saved runtime outcome is inconsistent"))),
                    }
                }else{RuntimeReservation::Pending};
                return Ok(Ok(result));
            }
            let valid=operation_id.split_once(':').is_some_and(|(instance,nonce)|instance==instance_id && !nonce.is_empty());
            if !valid {return Ok(Err(error(ErrorCode::OperationExpired,"unknown operation from an earlier instance cannot execute")));}
            if let Some(error)=preflight {return Ok(Err(error));}
            if let Err(error)=super::receipt::reserve(&tx,&instance_id,&operation_id,&serde_json::to_string(&caller.as_ref().map(|origin|origin.identity())).unwrap(),&fingerprint,&chrono::Utc::now().to_rfc3339(),admission) {
                if error.code==ErrorCode::StorageLimit {tx.commit()?; *pressure=true;}
                return Ok(Err(error));
            }
            tx.commit()?;
            Ok(Ok(RuntimeReservation::New))
        }).await
    }

    pub async fn finalize_runtime(
        &self,
        operation_id: String,
        outcome: Result<Value>,
    ) -> Result<()> {
        let instance = self.instance_id.clone();
        super::receipt::encode(&operation_id, &outcome)?;
        let saved = match outcome {
            Ok(result) => SavedOutcome {
                result: Some(result),
                error: None,
            },
            Err(error) => SavedOutcome {
                result: None,
                error: Some(error),
            },
        };
        self.database.call(move|db| {
            let changed=db.execute("UPDATE operation_receipt SET state=?3,result_ref_json=?4,reserved_bytes=0 WHERE operation_id=?1 AND instance_id=?2 AND state='pending'",params![operation_id,instance,if saved.error.is_some(){"failed"}else{"completed"},serde_json::to_string(&saved).unwrap()])?;
            if changed!=1 {return Ok(Err(error(ErrorCode::InvalidState,"runtime operation was not reserved or was already finalized")));}
            Ok(Ok(()))
        }).await.map_err(storage_error)?
    }

    /// Trusted human UI control: resolve and stop in one transaction, without
    /// consuming a generic operation receipt needed by external CLI replay.
    pub(crate) async fn stop_current_thread(&self, thread: String) -> Result<MutationResult> {
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                    [&thread],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Ok(Err(error(ErrorCode::ThreadNotFound, "Thread not found")));
                }
                let run: Option<String> = tx
                    .query_row(
                        "SELECT id FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",
                        [&thread],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(run) = run else {
                    return Ok(Err(error(ErrorCode::InvalidState, "No live Run to stop")));
                };
                let effects = stop_run(&tx, &run, &chrono::Utc::now().to_rfc3339());
                if effects.is_ok() {
                    tx.commit()?;
                }
                Ok(effects)
            })
            .await
            .map_err(storage_error)?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn accept_edit(
        &self,
        operation_id: String,
        thread: String,
        node: String,
        text: String,
        files: Vec<crate::attachments::PlannedAttachment>,
        args: ThreadCreateArgs,
    ) -> Result<MutationResult> {
        let content = crate::attachments::DraftContent::appended(text, files.len());
        self.accept_ordered_edit(
            operation_id,
            thread,
            node,
            content,
            files,
            args,
            DraftSendOptions::default(),
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn accept_ordered_edit(
        &self,
        operation_id: String,
        thread: String,
        node: String,
        content: crate::attachments::DraftContent,
        files: Vec<crate::attachments::PlannedAttachment>,
        args: ThreadCreateArgs,
        options: DraftSendOptions,
    ) -> Result<MutationResult> {
        content.validate(files.len())?;
        let store = self.clone();
        self.database.call(move|db|{
            let tx=db.transaction()?;
            let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM operation_receipt WHERE operation_id=?1 AND instance_id=?2 AND state='pending')",params![operation_id,store.instance_id],|r|r.get(0))?;
            if !pending{return Ok(Err(error(ErrorCode::InvalidState,"Edited request has no reserved operation")));}
            tx.execute_batch("SAVEPOINT edit_accept")?;
            let result=(||{
                let live:Option<String>=tx.query_row("SELECT id FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",[&thread],|r|r.get(0)).optional().map_err(sql_error)?;
                if let Some(run)=live{return Err(error(ErrorCode::InvalidState,"A Run is active; stop it or wait until it finishes before editing").details(json!({"run_id":run})));}
                let user:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM node n JOIN thread_path p ON p.node_id=n.id WHERE p.thread_id=?1 AND n.id=?2 AND n.deleted_reason IS NULL AND json_extract(n.payload_json,'$.role')='user')",params![thread,node],|r|r.get(0)).map_err(sql_error)?;
                if !user{return Err(error(ErrorCode::InvalidArgument,"Edit target must be an original user request on this Thread's selected path"));}
                let fork=uuid::Uuid::new_v4().to_string();let now=chrono::Utc::now().to_rfc3339();
                history::fork_thread(&tx,&ThreadForkArgs{ agents: None,thread_id:thread.clone(),node_id:node.clone(),cwd:args.cwd.clone()},&fork,&now)?;
                let mut path:Vec<String>=tx.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal").map_err(sql_error)?.query_map([&fork],|r|r.get(0)).map_err(sql_error)?.collect::<rusqlite::Result<_>>().map_err(sql_error)?;
                if path.pop().as_deref()!=Some(&node){return Err(error(ErrorCode::InvalidState,"Edit fork boundary changed"));}
                tx.execute("DELETE FROM attachment_selection WHERE thread_id=?1 AND attachment_id IN (SELECT id FROM attachment_ref WHERE source_node_id=?2)",params![fork,node]).map_err(sql_error)?;
                history::select_path(&tx,&fork,1,&path,&now).map_err(storage_error)?;
                if args.connection.is_some()||args.model.is_some()||args.effort.is_some(){store.set_thread(&tx,&ThreadSetArgs{ auto_compact_target_percent:None,summary_max_chars:None,agents: None,thread_id:Some(fork.clone()),connection:args.connection,model:args.model,effort:args.effort,cwd:None,title:None,auto_compact_at:None,segment_chars:None,reduce_rounds:None, parent_id: None},None,&now)?;}
                let revision:i64=history::selection_revision(&tx, &fork, history::ThreadScope::Any).map_err(sql_error)?;
                let mut result=store.send_content(&tx,&MessageSendArgs { human: true, resume_recovery: false,
 to_human: false, expect_reply: false, sender_label: None,thread_id:Some(fork),text:content.text.clone(),deliver:options.deliver},None,&operation_id,&now,SendContent { elements: content.new_source_elements(files.len())?, sender: MessageSender::recorded(None, None, true) })?;
                let message=result.result.get("message_id").and_then(Value::as_str).ok_or_else(||error(ErrorCode::InvalidState,"Accepted edit has no source node"))?;
                crate::attachments::add_tx(&tx,&crate::attachments::AddPlan{thread_id:result.result["thread_id"].as_str().unwrap().into(),selection_revision:revision,files},Some(message))?;
                result.result["source_thread_id"]=json!(thread);result.result["replaced_request_id"]=json!(node);
                if let Some(submission)=&options.submission {
                    super::pending_messages::submit_draft_tx(&tx,&operation_id,submission,&result.result,&now,&store.config).map_err(storage_error)?;
                }
                Ok(result)
            })();
            if result.is_err(){tx.execute_batch("ROLLBACK TO edit_accept")?;}
            tx.execute_batch("RELEASE edit_accept")?;
            let body=match &result{Ok(result)=>SavedOutcome{result:Some(result.result.clone()),error:None},Err(error)=>SavedOutcome{result:None,error:Some(error.clone())}};
            tx.execute("UPDATE operation_receipt SET state=?2,result_ref_json=?3,reserved_bytes=0 WHERE operation_id=?1 AND state='pending'",params![operation_id,if result.is_ok(){"completed"}else{"failed"},serde_json::to_string(&body).unwrap()])?;
            tx.commit()?;Ok(result)
        }).await.map_err(storage_error)?
    }

    pub async fn finalize_internal(&self, operation: String, outcome: Result<Value>) -> Result<()> {
        super::receipt::encode(&operation, &outcome)?;
        let saved = match outcome {
            Ok(result) => SavedOutcome {
                result: Some(result),
                error: None,
            },
            Err(error) => SavedOutcome {
                result: None,
                error: Some(error),
            },
        };
        self.database.call(move|db|{db.execute("UPDATE operation_receipt SET state=?2,result_ref_json=?3,reserved_bytes=0 WHERE operation_id=?1 AND state='pending'",params![operation,if saved.error.is_some(){"failed"}else{"completed"},serde_json::to_string(&saved).unwrap()])?;Ok(())}).await.map_err(storage_error)
    }

    fn execute_transaction(
        &self,
        connection: &mut Connection,
        request: &Request,
        caller: Option<&Origin>,
        plan: Option<&AgentsPlan>,
        fingerprint: &std::sync::OnceLock<Result<String>>,
        pressure: &mut bool,
    ) -> Result<MutationResult> {
        let fingerprint = request_fingerprint(fingerprint, request, caller)?;
        self.transact(
            connection,
            &request.operation_id,
            fingerprint,
            caller,
            Admission::for_command(&request.command),
            pressure,
            |tx, now| {
                if let Some(plan) = plan {
                    if let Some((thread, expected)) = &plan.source {
                        let revision: Option<i64> = tx.query_row("SELECT revision FROM thread WHERE id=?1 AND deleted_at IS NULL", [thread], |r|r.get(0)).optional().map_err(sql_error)?;
                        if revision != Some(*expected) { return Err(error(ErrorCode::OperationConflict, "Thread settings changed during AGENTS validation; retry with a new operation ID")); }
                    }
                    self.dispatch(tx, &plan.command, caller, &request.operation_id, now)
                } else {
                    self.dispatch(tx, &request.command, caller, &request.operation_id, now)
                }
            },
        )
    }

    /// TUI's first send creates its Thread and durable message atomically.
    pub async fn send_new_thread(
        &self,
        operation_id: String,
        args: ThreadCreateArgs,
        text: String,
    ) -> Result<MutationResult> {
        Command::ThreadCreate(args.clone()).validate()?;
        if operation_id.is_empty() {
            return Err(error(
                ErrorCode::InvalidArgument,
                "operation ID is required",
            ));
        }
        let fingerprint = hash(
            &serde_json::to_vec(
                &json!({"internal_operation":"first_send","args":args,"text":text,"caller":null}),
            )
            .map_err(|_| error(ErrorCode::InvalidArgument, "invalid first-send encoding"))?,
        );
        let store = self.clone();
        let args = Arc::new(args);
        let text = Arc::new(text);
        self.receipt_job(None, None, move |connection, pressure| {
            Ok(store.transact(
                connection,
                &operation_id,
                &fingerprint,
                None,
                Admission::Normal,
                pressure,
                |tx, now| {
                    let thread_id = store.create_thread(tx, &args, now)?;
                    store.send_content(
                        tx,
                        &MessageSendArgs {
                            human: true,
                            resume_recovery: false,
                            to_human: false,
                            expect_reply: false,
                            sender_label: None,
                            thread_id: Some(thread_id),
                            text: text.as_ref().clone(),
                            deliver: Deliver::Next,
                        },
                        None,
                        &operation_id,
                        now,
                        SendContent {
                            elements: vec![json!({"type":"input_text","text":text.as_str()})],
                            sender: MessageSender::recorded(None, None, true),
                        },
                    )
                },
            ))
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    fn transact(
        &self,
        connection: &mut Connection,
        operation_id: &str,
        fingerprint: &str,
        caller: Option<&Origin>,
        admission: Admission,
        pressure: &mut bool,
        operation: impl FnOnce(&Transaction<'_>, &str) -> Result<MutationResult>,
    ) -> Result<MutationResult> {
        let tx = connection.transaction().map_err(sql_error)?;
        if let Some(saved) = saved_operation(&tx, operation_id, fingerprint)? {
            return Ok(saved);
        }
        let Some((instance, nonce)) = operation_id.split_once(':') else {
            return Err(error(
                ErrorCode::OperationExpired,
                "operation ID has no active instance prefix",
            ));
        };
        if instance != self.instance_id || nonce.is_empty() {
            return Err(error(
                ErrorCode::OperationExpired,
                "unknown operation from an earlier instance cannot execute",
            ));
        }
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(error) = super::receipt::reserve(
            &tx,
            &self.instance_id,
            operation_id,
            &serde_json::to_string(&caller.map(Origin::identity)).unwrap(),
            fingerprint,
            &now,
            admission,
        ) {
            if error.code == ErrorCode::StorageLimit {
                tx.commit().map_err(sql_error)?;
                *pressure = true;
            }
            return Err(error);
        }
        tx.execute_batch("SAVEPOINT operation_mutation")
            .map_err(sql_error)?;
        let mut outcome = operation(&tx, &now);
        let projected = outcome.as_ref().map_err(Clone::clone).and_then(|r| {
            r.committed_error
                .clone()
                .map_or_else(|| Ok(r.result.clone()), Err)
        });
        if let Err(error) = super::receipt::encode(operation_id, &projected) {
            outcome = Err(error);
        }
        if outcome.is_err() {
            tx.execute_batch("ROLLBACK TO operation_mutation")
                .map_err(sql_error)?;
        }
        tx.execute_batch("RELEASE operation_mutation")
            .map_err(sql_error)?;
        let saved = match &outcome {
            Ok(result) if result.committed_error.is_some() => SavedOutcome {
                result: None,
                error: result.committed_error.clone(),
            },
            Ok(result) => SavedOutcome {
                result: Some(result.result.clone()),
                error: None,
            },
            Err(error) => SavedOutcome {
                result: None,
                error: Some(error.clone()),
            },
        };
        tx.execute("UPDATE operation_receipt SET state=?2,result_ref_json=?3,reserved_bytes=0 WHERE operation_id=?1",params![operation_id,if saved.error.is_none(){"completed"}else{"failed"},serde_json::to_string(&saved).unwrap()]).map_err(sql_error)?;
        tx.commit().map_err(sql_error)?;
        outcome.and_then(|result| match result.committed_error.clone() {
            Some(error) => Err(error),
            None => Ok(result),
        })
    }

    fn dispatch(
        &self,
        tx: &Transaction<'_>,
        command: &Command,
        caller: Option<&Origin>,
        operation_id: &str,
        now: &str,
    ) -> Result<MutationResult> {
        match command {
            Command::OwnerShutdown(args) => {
                if args.instance_id != self.instance_id {
                    return Err(error(
                        ErrorCode::ProtocolMismatch,
                        "Shutdown targets a different owner instance",
                    ));
                }
                Ok(MutationResult {
                    owner_shutdown: true,
                    result: json!({"state":"accepted","instance_id":self.instance_id,"shutdown_complete":false}),
                    ..MutationResult::default()
                })
            }

            Command::Status(_) => Err(error(
                ErrorCode::InvalidState,
                "Status uses the dedicated read-only projection",
            )),
            Command::ThreadCreate(args) => {
                let mut defaults = args.clone();
                defaults.agents = None;
                let id = self.create_thread(tx, &defaults, now)?;
                if let Some(path) = &args.agents {
                    self.set_agents_tx(tx, &id, path, caller, now)?;
                }
                let mut result = MutationResult::value(thread_details(tx, &id)?)?;
                result.changed_threads.push(id);
                Ok(result)
            }
            Command::ThreadFork(args) => {
                let id = uuid::Uuid::new_v4().to_string();
                let mut defaults = args.clone();
                defaults.agents = None;
                history::fork_thread(tx, &defaults, &id, now)?;
                if let Some(path) = &args.agents {
                    self.set_agents_tx(tx, &id, path, caller, now)?;
                }
                let mut result = MutationResult::value(thread_details(tx, &id)?)?;
                result.changed_threads.push(id);
                Ok(result)
            }
            Command::ThreadGet(args) => {
                let id = target(args.thread_id.as_deref(), caller)?;
                let mut details = thread_details(tx, &id)?;
                self.fill_effective(&mut details);
                details.usage_totals = Some(crate::usage_totals::read_logged(
                    tx,
                    &crate::usage_totals::Scope::Thread {
                        id,
                        children: false,
                    },
                    self.usage_context,
                    self.usage_diagnostics.as_ref(),
                )?);
                let compact = crate::readback::compactions(
                    tx,
                    &details.thread_id,
                    &crate::protocol::CompactListArgs {
                        thread_id: Some(details.thread_id.clone()),
                        since: None,
                        until: None,
                        limit: 1,
                    },
                )?;
                details.last_compaction =
                    compact["items"].as_array().and_then(|a| a.first()).cloned();
                MutationResult::value(details)
            }
            Command::CompactList(args) => {
                let id = target(args.thread_id.as_deref(), caller)?;
                MutationResult::value(crate::readback::compactions(tx, &id, args)?)
            }
            Command::ShellList(args) => {
                MutationResult::value(crate::readback::shells(tx, args, &self.instance_id)?)
            }
            Command::UsageGet(args) => {
                let mut args = args.clone();
                if let Some(node_id) = &args.since_node_id {
                    let scope_thread = if let Some(turn) = &args.turn_id {
                        tx.query_row("SELECT thread_id FROM turn WHERE id=?1", [turn], |r| {
                            r.get::<_, String>(0)
                        })
                        .optional()
                        .map_err(sql_error)?
                        .ok_or_else(|| {
                            error(ErrorCode::TargetUnavailable, "Usage scope is unavailable")
                        })?
                    } else if let Some(run) = &args.run_id {
                        tx.query_row("SELECT thread_id FROM run WHERE id=?1", [run], |r| {
                            r.get::<_, String>(0)
                        })
                        .optional()
                        .map_err(sql_error)?
                        .ok_or_else(|| {
                            error(ErrorCode::TargetUnavailable, "Usage scope is unavailable")
                        })?
                    } else {
                        target(args.thread_id.as_deref(), caller)?
                    };
                    let at: String = tx.query_row("SELECT n.created_at FROM node n JOIN thread_path p ON p.node_id=n.id AND p.thread_id=?2 WHERE n.id=?1 AND n.deleted_reason IS NULL", params![node_id,scope_thread], |r| r.get(0)).optional().map_err(sql_error)?
                        .ok_or_else(|| error(ErrorCode::TargetUnavailable, "Usage start node is unavailable on the selected Thread path"))?;
                    args.since = Some(at.into_boxed_str());
                    args.since_node_id = None;
                    // Grouped requests otherwise default to every Thread; anchor them to this path.
                    if args.group_by.is_some()
                        && args.thread_id.is_none()
                        && args.run_id.is_none()
                        && args.turn_id.is_none()
                    {
                        args.thread_id = Some(scope_thread.into_boxed_str());
                    }
                }
                if let Some(attempt) = &args.attempt_id {
                    return MutationResult::value(crate::usage_totals::read_attempt(tx, attempt)?);
                }
                if args.group_by.is_some() {
                    return MutationResult::value(crate::usage_totals::breakdown::read(
                        tx,
                        &args,
                        &self.instance_id,
                    )?);
                }
                let scope = if let Some(id) = &args.turn_id {
                    crate::usage_totals::Scope::Turn(id.clone())
                } else if let Some(id) = &args.run_id {
                    crate::usage_totals::Scope::Run(id.clone())
                } else {
                    crate::usage_totals::Scope::Thread {
                        id: target(args.thread_id.as_deref(), caller)?,
                        children: args.include_children,
                    }
                };
                MutationResult::value(crate::usage_totals::read_period_logged(
                    tx,
                    &scope,
                    self.usage_context,
                    self.usage_diagnostics.as_ref(),
                    &crate::readback::Window::new(args.since.as_deref(), args.until.as_deref())?,
                )?)
            }
            Command::ThreadList(args) => MutationResult::value(thread_list(tx, args)?),
            Command::ThreadTags(args) => MutationResult::value(tags::names(tx, args)?),
            Command::ThreadCwds(args) => MutationResult::value(tags::cwds(tx, args)?),
            Command::ThreadSet(args) => {
                let mut result = self.set_thread(tx, args, caller, now)?;
                if let Some(thread) = result.result["thread_id"].as_str().map(str::to_owned) {
                    let details = thread_details(tx, &thread)?;
                    let mut details = details;
                    self.fill_effective(&mut details);
                    result.result["effective_auto_compact_at"] =
                        serde_json::json!(details.effective_auto_compact_at);
                    result.result["effective_auto_compact_target_percent"]=serde_json::json!(details.effective_auto_compact_target_percent);
                    result.result["effective_summary_max_chars"]=serde_json::json!(details.effective_summary_max_chars);
                    result.result["segment_chars"] = serde_json::json!(details.segment_chars);
                    result.result["effective_segment_chars"] =
                        serde_json::json!(details.effective_segment_chars);
                    result.result["reduce_rounds"] = serde_json::json!(details.reduce_rounds);
                    result.result["effective_reduce_rounds"] =
                        serde_json::json!(details.effective_reduce_rounds);
                }
                Ok(result)
            }
            Command::ThreadDelete(args) => {
                let result = delete_thread(tx, args, now);
                if result.is_ok() {
                    // The single writer runs no maintenance unit before this job
                    // commits or rolls back, so marking here cannot be missed;
                    // a rollback only costs one idle pass.
                    let signals = self.database.signals();
                    signals.mark_all(&crate::maintenance::Work::DELETION);
                    signals.mark_all(&[
                        crate::maintenance::Work::Managed,
                        crate::maintenance::Work::Drafts,
                    ]);
                }
                result
            }
            Command::ThreadPrune(args) => {
                let id = target(args.thread_id.as_deref(), caller)?;
                let value =
                    history::prune::prune_thread(tx, &id, &args.before_node_id, args.dry_run)?;
                if !args.dry_run && value["gc_job_id"].is_string() {
                    // Same signal as thread delete: wake the existing history GC
                    // (and its derived-data followers). No timing is promised.
                    self.database
                        .signals()
                        .mark_all(&crate::maintenance::Work::DELETION);
                }
                let mut result = MutationResult::value(value)?;
                if !args.dry_run {
                    // Pruning tombstones originals the Thread read; observers re-read (#243).
                    result.changed_threads.push(id);
                }
                Ok(result)
            }
            Command::ThreadBranches(args) => {
                let id = target(args.thread_id.as_deref(), caller)?;
                MutationResult::value(history::branches::branches(tx, &id, args)?)
            }
            Command::ThreadRewind(args) => {
                let id = target(args.thread_id.as_deref(), caller)?;
                let branch = uuid::Uuid::new_v4().to_string();
                let mut value = history::rewind::rewind_thread(
                    tx,
                    &id,
                    &args.node_id,
                    args.exclude_node,
                    args.dry_run,
                    &branch,
                    now,
                )?;
                let stored: String = tx
                    .query_row("SELECT title FROM thread WHERE id=?1", [&id], |r| r.get(0))
                    .map_err(sql_error)?;
                value["title"] = json!(thread_display_title(tx, &id, &stored)?);
                let mut result = MutationResult::value(value)?;
                if !args.dry_run {
                    result.changed_threads.push(id);
                    result.changed_threads.push(branch);
                }
                Ok(result)
            }
            Command::ThreadArchive(args) => archive::run(tx, args, caller, true, now),
            Command::ThreadUnarchive(args) => archive::run(tx, args, caller, false, now),
            Command::ThreadTag(args) => tags::run(tx, args, caller),
            Command::ThreadDeletePreview(args) => {
                MutationResult::value(crate::deletion::preview(tx, args)?)
            }
            Command::MessageReply(args) => self.reply(tx, args, caller, operation_id, now),
            Command::MessageSend(args) => self.send(tx, args, caller, operation_id, now),
            Command::MessageAsk(args) => self.ask_human(tx, args, caller, operation_id, now),
            Command::MessageGet(args) => {
                MutationResult::value(message_details(tx, &args.message_id)?)
            }
            Command::MessageCancel(args) => {
                let (cancelled, changed) =
                    super::pending_messages::cancel_tx(tx, args, caller, now)
                        .map_err(storage_error)?;
                let mut result = MutationResult::value(cancelled)?;
                result.changed_threads = changed;
                result.quiet_wake_threads =
                    super::reply::cancellation_wakes_tx(tx, &args.message_ids)
                        .map_err(sql_error)?;
                Ok(result)
            }
            Command::OperationGet(args) => MutationResult::value(operation_details(
                tx,
                &args.operation_id,
                &self.instance_id,
            )?),
            Command::RunGet(args) => {
                let mut details = run_details(tx, &args.run_id)?;
                details.usage_totals = Some(saved_usage_for_detail(
                    tx,
                    &crate::usage_totals::Scope::Run(args.run_id.clone()),
                    self.usage_context,
                    self.usage_diagnostics.as_ref(),
                )?);
                MutationResult::value(details)
            }
            Command::TurnGet(args) => {
                let mut details = turn_details(tx, &args.turn_id)?;
                details.usage_totals = Some(saved_usage_for_detail(
                    tx,
                    &crate::usage_totals::Scope::Turn(args.turn_id.clone()),
                    self.usage_context,
                    self.usage_diagnostics.as_ref(),
                )?);
                MutationResult::value(details)
            }
            Command::RunStop(args) => stop_run(tx, &args.run_id, now),
            Command::RunRetry(args) => retry_run(tx, &args.run_id, caller, now),
            Command::CompactPreview(_) => Err(error(
                ErrorCode::InvalidState,
                "Compact preview uses its dedicated read-only projection",
            )),
            Command::Compact(args) => {
                queue_compact_tx(tx, args.thread_id.as_deref(), caller, operation_id, None)
            }
            Command::ContextEdit(args) => queue_compact_tx(
                tx,
                args.thread_id.as_deref(),
                caller,
                operation_id,
                Some(args),
            ),
            Command::AttachmentList(args) => MutationResult::value(crate::attachments::list_tx(
                tx,
                &target(args.thread_id.as_deref(), caller)?,
            )?),
            Command::AttachmentRemove(args) => {
                let thread = target(args.thread_id.as_deref(), caller)?;
                let changed = crate::attachments::remove_tx(tx, &thread, &args.attachment_id)?;
                let mut result = MutationResult::value(
                    json!({"attachment_id":args.attachment_id,"removed":true}),
                )?;
                if changed {
                    result.changed_threads.push(thread);
                }
                Ok(result)
            }
            Command::HistoryList(args) => budget_history(
                operation_id,
                history::list_nodes(tx, args)?,
                crate::history::message_budget::Slots::Nodes,
            ),
            Command::HistoryWindow(args) => budget_history(
                operation_id,
                history::window(tx, args)?,
                crate::history::message_budget::Slots::Nodes,
            ),
            Command::HistoryGet(args) => {
                let (body, parts) = history::read_node(tx, args)?;
                let mut value = serde_json::to_value(body).map_err(|_| {
                    error(ErrorCode::InvalidState, "history body is not serializable")
                })?;
                if let (Some(parts), Some(object)) = (parts, value.as_object_mut()) {
                    object.insert("parts".into(), parts);
                }
                budget_history(
                    operation_id,
                    value,
                    crate::history::message_budget::Slots::Body,
                )
            }
            Command::HistoryRelations(args) => MutationResult::value(history::relations(tx, args)?),
            _ => Err(error(
                ErrorCode::InvalidState,
                "this operation requires its runtime service",
            )),
        }
    }

    async fn prepare_agents(
        &self,
        request: &Request,
        caller: &Option<Origin>,
    ) -> Result<Option<AgentsPlan>> {
        let (path, cwd, source) = match &request.command {
            Command::ThreadCreate(a) => (a.agents.clone(), a.cwd.clone(), a.parent_id.clone()),
            Command::ThreadFork(a) => (a.agents.clone(), a.cwd.clone(), Some(a.thread_id.clone())),
            Command::ThreadSet(a) => (
                a.agents.clone(),
                a.cwd.clone(),
                Some(target(a.thread_id.as_deref(), caller.as_ref())?),
            ),
            _ => return Ok(None),
        };
        let Some(path) = path else {
            return Ok(None);
        };
        if self.replay(request, caller).await?.is_some() {
            return Ok(None);
        }
        let source_id = source.clone();
        let startup = self.startup_cwd.clone();
        let (cwd, source) = self
            .database
            .call(move |db| {
                let (base, source) = if let Some(id) = source_id {
                    let value: Option<(String, i64)> = db
                        .query_row(
                            "SELECT cwd,revision FROM thread WHERE id=?1 AND deleted_at IS NULL",
                            [&id],
                            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                        )
                        .optional()?;
                    let Some((base, revision)) = value else {
                        return Ok(Err(error(ErrorCode::ThreadNotFound, "Thread not found")));
                    };
                    (PathBuf::from(base), Some((id, revision)))
                } else {
                    (startup, None)
                };
                Ok(
                    normalize(cwd.as_deref().map(Path::new).unwrap_or(&base), &base)
                        .map(|cwd| (cwd, source)),
                )
            })
            .await
            .map_err(storage_error)??;
        let path = normalize(Path::new(&path), &cwd)?;
        let prepared = self.instructions.prepare(cwd, path.clone()).await?;
        if prepared.is_none() {
            return Err(error(
                ErrorCode::InvalidArgument,
                "Explicit AGENTS path does not exist",
            ));
        }
        let path = path
            .to_str()
            .ok_or_else(|| error(ErrorCode::InvalidArgument, "AGENTS path must be Unicode"))?
            .to_owned();
        let mut command = request.command.clone();
        match &mut command {
            Command::ThreadCreate(a) => a.agents = Some(path),
            Command::ThreadSet(a) => a.agents = Some(path),
            Command::ThreadFork(a) => a.agents = Some(path),
            _ => unreachable!(),
        }
        Ok(Some(AgentsPlan { command, source }))
    }

    fn set_agents_tx(
        &self,
        tx: &Transaction<'_>,
        thread: &str,
        path: &str,
        caller: Option<&Origin>,
        now: &str,
    ) -> Result<()> {
        let before: String = tx
            .query_row(
                "SELECT agents_path FROM thread WHERE id=?1",
                [thread],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if before == path {
            return Ok(());
        }
        let excluded_turn: Option<String> = tx.query_row("SELECT id FROM turn WHERE thread_id=?1 AND state IN ('preparing','running','waiting','recovery_wait') ORDER BY created_at DESC,rowid DESC LIMIT 1", [thread], |r|r.get(0)).optional().map_err(sql_error)?;
        tx.execute(
            "UPDATE thread SET agents_path=?2 WHERE id=?1",
            params![thread, path],
        )
        .map_err(sql_error)?;
        super::context::record(tx, thread, "agents_reference", actor(caller), json!({"before":{"path":before},"after":{"path":path},"excluded_turn_id":excluded_turn}), now).map_err(sql_error)?;
        Ok(())
    }

    fn create_thread(
        &self,
        tx: &Transaction<'_>,
        args: &ThreadCreateArgs,
        now: &str,
    ) -> Result<String> {
        let config = &self.config.current();
        let inherited = if let Some(parent) = &args.parent_id {
            Some(tx.query_row("SELECT cwd,agents_path,profile_id,model_id,effort,auto_compact_at,segment_chars,reduce_rounds,auto_compact_target_percent,summary_max_chars FROM thread WHERE id=?1 AND deleted_at IS NULL",[parent],|row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<i64>>(5)?,row.get::<_,Option<i64>>(6)?,row.get::<_,Option<i64>>(7)?,row.get::<_,Option<i64>>(8)?,row.get::<_,Option<i64>>(9)?))).optional().map_err(sql_error)?.ok_or_else(|| error(ErrorCode::ThreadNotFound,"parent Thread not found"))?)
        } else {
            None
        };
        let base = inherited
            .as_ref()
            .map_or(self.startup_cwd.as_path(), |p| Path::new(&p.0));
        let cwd = normalize(args.cwd.as_deref().map(Path::new).unwrap_or(base), base)?;
        let agents = inherited.as_ref().map_or_else(
            || cwd.join("AGENTS.md").to_string_lossy().into_owned(),
            |parent| parent.1.clone(),
        );
        let profile = args
            .connection
            .clone()
            .or_else(|| inherited.as_ref().map(|p| p.2.clone()))
            .or_else(|| config.default_connection.clone())
            .ok_or_else(|| {
                crate::config_setup::enrich(
                    config,
                    None,
                    args.model.as_deref().or(config.default_model.as_deref()),
                    error(
                        ErrorCode::ConfigInvalid,
                        "set a default connection or choose --connection",
                    ),
                )
            })?;
        let connection = config.connections.get(&profile).ok_or_else(|| {
            crate::config_setup::enrich(
                config,
                Some(&profile),
                args.model.as_deref().or(config.default_model.as_deref()),
                error(
                    ErrorCode::ConfigInvalid,
                    "selected connection is not configured",
                ),
            )
        })?;
        let model = args
            .model
            .clone()
            .or_else(|| inherited.as_ref().map(|p| p.3.clone()))
            .or_else(|| config.default_model.clone())
            .ok_or_else(|| {
                crate::config_setup::enrich(
                    config,
                    Some(&profile),
                    None,
                    error(
                        ErrorCode::ConfigInvalid,
                        "set a default model or choose --model",
                    ),
                )
            })?;
        let effort = args
            .effort
            .clone()
            .or_else(|| inherited.as_ref().and_then(|p| p.4.clone()))
            .or_else(|| {
                if inherited.is_none() {
                    connection.default_effort.clone()
                } else {
                    None
                }
            })
            .filter(|value| !value.is_empty());
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,effort,updated_at,auto_compact_at,segment_chars,reduce_rounds,auto_compact_target_percent,summary_max_chars) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",params![id,args.parent_id,cwd.to_string_lossy(),agents,profile,model,effort,now,inherited.as_ref().and_then(|p|p.5),inherited.as_ref().and_then(|p|p.6),inherited.as_ref().and_then(|p|p.7),inherited.as_ref().and_then(|p|p.8),inherited.as_ref().and_then(|p|p.9)]).map_err(sql_error)?;
        bump_roots(tx)?;
        Ok(id)
    }

    /// Values in effect for the Thread: its own override, then the connection, then `[compaction]`.
    fn fill_effective(&self, details: &mut ThreadDetails) {
        let config = &self.config.current();
        let connection = config.connections.get(&details.connection);
        let compaction = &config.compaction;
        details.effective_auto_compact_at =
            Some(compaction.auto_at_tokens_for(details.auto_compact_at, connection));
        details.effective_auto_compact_target_percent=Some(crate::config::target_percent_for(details.auto_compact_target_percent,connection));
        details.effective_summary_max_chars=Some(crate::config::summary_chars_for(details.summary_max_chars,connection));
        details.effective_segment_chars = Some(
            compaction.segment_chars_for(details.segment_chars.map(|v| v as usize), connection)
                as u64,
        );
        details.effective_reduce_rounds = Some(
            compaction.reduce_rounds_for(details.reduce_rounds.map(|v| v as usize), connection)
                as u64,
        );
    }

    fn set_thread(
        &self,
        tx: &Transaction<'_>,
        args: &ThreadSetArgs,
        caller: Option<&Origin>,
        now: &str,
    ) -> Result<MutationResult> {
        let config = &self.config.current();
        let id = target(args.thread_id.as_deref(), caller)?;
        let current = thread_details(tx, &id)?;
        let title = args.title.as_deref().map(str::trim);
        // Parent first, in the same transaction: a later failure rolls the move back.
        let moved = match args.parent_id.as_deref() {
            Some(requested) => reparent::apply(tx, &id, requested, caller, now)?,
            None => None,
        };
        if let Some(value)=&args.auto_compact_target_percent {
            let value=crate::protocol::validation::parse_target_percent(value)?;
            tx.execute("UPDATE thread SET auto_compact_target_percent=?2 WHERE id=?1",params![id,value]).map_err(sql_error)?;
        }
        if let Some(value)=&args.summary_max_chars {
            let value=crate::protocol::validation::parse_summary_chars(value)?;
            tx.execute("UPDATE thread SET summary_max_chars=?2 WHERE id=?1",params![id,value]).map_err(sql_error)?;
        }
        if let Some(value) = &args.auto_compact_at {
            let value = crate::protocol::validation::parse_auto_compact_at(value)?;
            tx.execute(
                "UPDATE thread SET auto_compact_at=?2 WHERE id=?1",
                params![id, value],
            )
            .map_err(sql_error)?;
        }
        if let Some(value) = &args.segment_chars {
            let value = crate::protocol::validation::parse_segment_chars(value)?;
            tx.execute(
                "UPDATE thread SET segment_chars=?2 WHERE id=?1",
                params![id, value],
            )
            .map_err(sql_error)?;
        }
        if let Some(value) = &args.reduce_rounds {
            let value = crate::protocol::validation::parse_reduce_rounds(value)?;
            tx.execute(
                "UPDATE thread SET reduce_rounds=?2 WHERE id=?1",
                params![id, value],
            )
            .map_err(sql_error)?;
        }
        // A move alone is organization: no updated_at change, so the list does not jump.
        let moved_only = args.parent_id.is_some()
            && title.is_none()
            && args.connection.is_none()
            && args.model.is_none()
            && args.cwd.is_none()
            && args.effort.is_none()
            && args.agents.is_none()
            && args.auto_compact_at.is_none()
            && args.auto_compact_target_percent.is_none()
            && args.summary_max_chars.is_none()
            && args.segment_chars.is_none()
            && args.reduce_rounds.is_none();
        if moved_only {
            let mut result = MutationResult::value(thread_details(tx, &id)?)?;
            if let Some(moved) = &moved {
                reparent::add_changed(&mut result, &id, moved);
                reparent::annotate(tx, &mut result, &id, moved)?;
            }
            return Ok(result);
        }
        if args.connection.is_none()
            && args.model.is_none()
            && args.cwd.is_none()
            && args.effort.is_none()
            && args.agents.is_none()
        {
            tx.execute("UPDATE thread SET title=COALESCE(?2,title),revision=revision+1,updated_at=?3 WHERE id=?1",params![id,title,now]).map_err(sql_error)?;
            let mut result = MutationResult::value(thread_details(tx, &id)?)?;
            result.changed_threads.push(id.clone());
            if let Some(moved) = &moved {
                reparent::add_changed(&mut result, &id, moved);
                reparent::annotate(tx, &mut result, &id, moved)?;
            }
            return Ok(result);
        }
        let profile = args.connection.as_deref().unwrap_or(&current.connection);
        if !config.connections.contains_key(profile) {
            return Err(error(
                ErrorCode::ConfigInvalid,
                "selected connection is not configured",
            ));
        }
        let model = args.model.as_deref().unwrap_or(&current.model);
        let cwd = normalize(
            args.cwd
                .as_deref()
                .map(Path::new)
                .unwrap_or_else(|| Path::new(&current.cwd)),
            Path::new(&current.cwd),
        )?;
        let effort = match args.effort.as_deref() {
            Some("") => None,
            Some(value) => Some(value),
            None => current.effort.as_deref(),
        };
        let changed = profile != current.connection || model != current.model;
        if changed {crate::compaction::a_state::scope_tx(tx,&id,profile,model).map_err(storage_error)?;}
        tx.execute("UPDATE thread SET cwd=?2,profile_id=?3,model_id=?4,effort=?5,revision=revision+1,profile_revision=profile_revision+1,reasoning_epoch=reasoning_epoch+?6,updated_at=?7,title=COALESCE(?8,title) WHERE id=?1",params![id,cwd.to_string_lossy(),profile,model,effort,i64::from(changed),now,title]).map_err(sql_error)?;
        if let Some(path) = &args.agents {
            self.set_agents_tx(tx, &id, path, caller, now)?;
        }
        let after = thread_details(tx, &id)?;
        if current.cwd != after.cwd
            || current.connection != after.connection
            || current.model != after.model
            || current.effort != after.effort
        {
            use super::notification::{Fact, Setting};
            let mut changed_fields = Vec::with_capacity(4);
            if current.cwd != after.cwd {
                changed_fields.push(Setting::Cwd);
            }
            if current.connection != after.connection {
                changed_fields.push(Setting::Connection);
            }
            if current.model != after.model {
                changed_fields.push(Setting::Model);
            }
            if current.effort != after.effort {
                changed_fields.push(Setting::Effort);
            }
            super::context::record_fact(tx,&id,"thread_settings",actor(caller),json!({"before":{"cwd":current.cwd,"connection":current.connection,"model":current.model,"effort":current.effort},"after":{"cwd":after.cwd,"connection":after.connection,"model":after.model,"effort":after.effort}}),now,Fact::ThreadSettingsChanged{changed_fields}).map_err(sql_error)?;
        }
        let mut result = MutationResult::value(after)?;
        result.changed_threads.push(id.clone());
        if let Some(moved) = &moved {
            reparent::add_changed(&mut result, &id, moved);
            reparent::annotate(tx, &mut result, &id, moved)?;
        }
        Ok(result)
    }

    pub(super) fn send(
        &self,
        tx: &Transaction<'_>,
        args: &MessageSendArgs,
        caller: Option<&Origin>,
        operation_id: &str,
        now: &str,
    ) -> Result<MutationResult> {
        self.send_content(
            tx,
            args,
            caller,
            operation_id,
            now,
            SendContent {
                elements: vec![json!({"type":"input_text","text":args.text})],
                sender: MessageSender::recorded(caller, args.sender_label.clone(), args.human),
            },
        )
    }
    fn ask_human(
        &self,
        tx: &Transaction<'_>,
        args: &crate::protocol::MessageAskArgs,
        caller: Option<&Origin>,
        operation_id: &str,
        now: &str,
    ) -> Result<MutationResult> {
        if caller.is_none() || args.thread_id.is_some() {
            return Err(error(
                ErrorCode::InvalidArgument,
                "human questions require the current model Thread",
            ));
        }
        self.send(
            tx,
            &MessageSendArgs {
                human: false,
                resume_recovery: false,
                to_human: true,
                expect_reply: true,
                sender_label: None,
                thread_id: None,
                text: args.text.clone(),
                deliver: Deliver::Next,
            },
            caller,
            operation_id,
            now,
        )
    }
    fn send_content(
        &self,
        tx: &Transaction<'_>,
        args: &MessageSendArgs,
        caller: Option<&Origin>,
        operation_id: &str,
        now: &str,
        content: SendContent,
    ) -> Result<MutationResult> {
        validate_sender_label(content.sender.label.as_deref())?;
        if args.text.len() as u64 > self.config.limits.draft_bytes {
            return Err(error(
                ErrorCode::InputLimit,
                "message exceeds configured input limit",
            ));
        }
        if args.to_human
            && (!args.expect_reply
                || args.human
                || caller.is_none()
                || args.thread_id.is_some()
                || args.deliver != Deliver::Next)
        {
            return Err(error(
                ErrorCode::InvalidArgument,
                "human questions require a model origin, the current Thread, and default delivery",
            ));
        }
        let id = target(args.thread_id.as_deref(), caller)?;
        if args.to_human {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                    [&id],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            if !exists {
                return Err(error(ErrorCode::ThreadNotFound, "target Thread not found"));
            }
        }
        if args.expect_reply {
            let limit = crate::protocol::MAX_PENDING_REPLY_DUTIES;
            let pending: i64 = tx.query_row(
                "SELECT count(*) FROM (SELECT 1 FROM receipt INDEXED BY receipt_reply_pending \
                 WHERE target_thread_id=?1 AND json_extract(result_json,'$.reply.state')='pending' LIMIT ?2)",
                params![id, (limit + 1) as i64],
                |row| row.get(0),
            ).map_err(sql_error)?;
            if pending >= limit as i64 {
                return Err(ApiError::new(
                    ErrorCode::ReplyLimit,
                    format!("target Thread has {pending} pending reply duties (limit {limit}); reply to or cancel one before sending another"),
                ).details(json!({"limit":limit,"current_count":pending}))
                 .hint("Reply to or cancel an outstanding request in the target Thread, then retry; the rejected message was not accepted"));
            }
        }
        let MessageAssignment {
            run: assigned_run,
            turn: assigned_turn,
            assignment,
            status,
            reason,
            wake,
        } = if args.to_human {
            MessageAssignment {
                run: None,
                turn: None,
                assignment: Assignment::Pending,
                status: DeliveryStatus::Accepted,
                reason: None,
                wake: false,
            }
        } else {
            assign_message(
                tx,
                &id,
                caller,
                args.deliver,
                args.human,
                args.resume_recovery,
                now,
            )?
        };
        let message_id = uuid::Uuid::new_v4().to_string();
        let role = if args.to_human { "assistant" } else { "user" };
        let payload = json!({"role":role,"content":content.elements});
        let body = payload.to_string();
        tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![message_id,assigned_turn,if args.to_human {"assistant"} else {"user"},now,body,body.len() as i64,hash(body.as_bytes())]).map_err(sql_error)?;
        self.index
            .authorize_new_tx(tx, std::slice::from_ref(&message_id))?;
        let receipt_id = uuid::Uuid::new_v4().to_string();
        let state = if args.to_human {
            "applied"
        } else {
            match status {
                DeliveryStatus::Accepted => "accepted",
                DeliveryStatus::Held => "held",
                _ => unreachable!(),
            }
        };
        let accepted = MessageAccepted {
            to_human: args.to_human,
            expect_reply: args.expect_reply,
            message_id: message_id.clone(),
            thread_id: id.clone(),
            delivery_status: status,
            assignment,
            deliver: args.deliver,
            run_id: assigned_run,
            turn_id: assigned_turn,
        };
        let mut saved_acceptance = serde_json::to_value(&accepted)
            .map_err(|_| error(ErrorCode::InvalidState, "Cannot encode message acceptance"))?;
        if args.expect_reply {
            saved_acceptance["reply"] = json!({"state":"pending"});
        }
        tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,updated_at,hold_reason,result_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11)",params![receipt_id,operation_id,serde_json::to_string(&content.sender).unwrap(),id,message_id,accepted.run_id,accepted.turn_id,state,now,reason,saved_acceptance.to_string()]).map_err(sql_error)?;
        if args.to_human {
            crate::control::db::archive::clear_for_question(tx, &id, now).map_err(sql_error)?;
            tx.execute(
                "INSERT INTO thread_path(thread_id,ordinal,node_id) SELECT ?1,COALESCE(MAX(ordinal)+1,0),?2 FROM thread_path WHERE thread_id=?1",
                params![id, message_id],
            ).map_err(sql_error)?;
            tx.execute(
                "UPDATE thread SET cursor_node_id=?2,selection_revision=selection_revision+1 WHERE id=?1",
                params![id, message_id],
            ).map_err(sql_error)?;
        }
        tx.execute(
            "UPDATE thread SET revision=revision+1,updated_at=?2 WHERE id=?1",
            params![id, now],
        )
        .map_err(sql_error)?;
        bump_roots(tx)?;
        let accepted_thread = accepted.thread_id.clone();
        let mut result = MutationResult::value(accepted)?;
        if wake {
            if args.deliver == Deliver::Now {
                result.wake_threads.push(id);
            } else {
                result.quiet_wake_threads.push(id);
            }
        } else {
            // Queued or held messages change saved delivery state without a wake (#243).
            result.changed_threads.push(id);
        }
        if args.to_human && !result.changed_threads.contains(&accepted_thread) {
            result.changed_threads.push(accepted_thread);
        }
        Ok(result)
    }
}

/// Assignment and explicit resume policy, applied in the send transaction.
struct MessageAssignment {
    run: Option<String>,
    turn: Option<String>,
    assignment: Assignment,
    status: DeliveryStatus,
    reason: Option<&'static str>,
    wake: bool,
}

fn assign_message(
    tx: &Transaction<'_>,
    id: &str,
    caller: Option<&Origin>,
    deliver: Deliver,
    human: bool,
    resume_recovery: bool,
    now: &str,
) -> Result<MessageAssignment> {
    let _parent: Option<String> = tx
        .query_row(
            "SELECT parent_thread_id FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| error(ErrorCode::ThreadNotFound, "target Thread not found"))?;
    let run: Option<(String, String)> = tx
        .query_row(
            "SELECT id,state FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let mut assigned_run = None;
    let mut assigned_turn = None;
    let mut assignment = Assignment::Pending;
    let mut status = DeliveryStatus::Accepted;
    let mut reason = None;
    let mut wake = false;
    if let Some((run_id, state)) = run {
        // A Run already in recovery wait gets ordinary input, as at entry.
        let recovering: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM turn WHERE run_id=?1 AND state='recovery_wait')",
                [&run_id],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if state == "stopping" {
            status = DeliveryStatus::Held;
            reason = Some("stopping");
        } else if deliver == Deliver::Complete && !recovering {
            // Revisit: unassigned until this Run ends (see control::revisit).
            status = DeliveryStatus::Held;
            reason = Some(super::revisit::RUN_COMPLETION);
        } else {
            assigned_run = Some(run_id.clone());
            let turn:Option<(String,String,i64)>=tx.query_row("SELECT id,state,generation FROM turn WHERE run_id=?1 AND state IN ('preparing','running','waiting','recovery_wait')",[&run_id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(sql_error)?;
            if let Some((turn_id, state, generation)) = turn {
                assignment = Assignment::ExistingTurn;
                if state == "recovery_wait" && !human && !resume_recovery {
                    status = DeliveryStatus::Held;
                    reason = Some("recovery_wait");
                } else {
                    if state == "recovery_wait" {
                        if !super::state::resume_recovery_turn_tx(
                            tx, id, &run_id, &turn_id, generation, now,
                        )
                        .map_err(storage_error)?
                        {
                            return Err(error(
                                ErrorCode::OperationConflict,
                                "Turn changed before explicit recovery resume",
                            ));
                        }
                        super::context::record_fact(
                            tx,
                            id,
                            "recovery_resumed",
                            actor(caller),
                            json!({"run_id":run_id,"turn_id":turn_id}),
                            now,
                            super::notification::Fact::RecoveryResumed,
                        )
                        .map_err(sql_error)?;
                        let still_recovering: bool = tx
                            .query_row(
                                "SELECT state='recovery_wait' FROM turn WHERE id=?1",
                                [&turn_id],
                                |r| r.get(0),
                            )
                            .map_err(sql_error)?;
                        if still_recovering && !human {
                            status = DeliveryStatus::Held;
                            reason = Some("recovery_wait");
                        }
                    }
                    wake = true;
                }
                assigned_turn = Some(turn_id);
            } else {
                let turn_id =
                    super::state::create_turn_tx(tx, id, &run_id, now, now).map_err(sql_error)?;
                assigned_turn = Some(turn_id);
                assignment = Assignment::NewTurn;
                wake = true;
            }
        }
    } else {
        let (run_id, turn_id) = start_run_tx(tx, id, actor(caller), now).map_err(sql_error)?;
        // A human's new instruction resumes unassigned saved input.
        // An independent model send does not silently admit recovery input.
        // Previously assigned receipts keep their original Run/Turn forever.
        if caller.is_none() {
            super::message_delivery::recover_on_resume_tx(
                tx,
                id,
                &run_id,
                &turn_id,
                actor(caller),
                now,
            )
            .map_err(storage_error)?;
            if super::message_delivery::interrupted_remaining(tx, id).map_err(sql_error)? {
                tx.execute("UPDATE turn SET state='recovery_wait',recovery_reason_json='{\"reason\":\"held_messages_remaining\"}' WHERE id=?1", [&turn_id]).map_err(sql_error)?;
                status = DeliveryStatus::Held;
                reason = Some("recovery_wait");
            }
        }
        assigned_run = Some(run_id);
        assigned_turn = Some(turn_id);
        assignment = Assignment::NewRun;
        wake = true;
    }
    Ok(MessageAssignment {
        run: assigned_run,
        turn: assigned_turn,
        assignment,
        status,
        reason,
        wake,
    })
}

/// Shared ordinary Run activation for messages and explicit reply outcome delivery.
pub(super) fn start_run_tx(
    tx: &Connection,
    thread: &str,
    source: Value,
    at: &str,
) -> rusqlite::Result<(String, String)> {
    let run = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,?2,'live',?3)",
        params![run, thread, at],
    )?;
    super::context::record(
        tx,
        thread,
        "run_started",
        source.clone(),
        json!({"run_id":run}),
        at,
    )?;
    archive::clear_for_new_run(tx, thread, source, at)?;
    let turn = super::state::create_turn_tx(tx, thread, &run, at, at)?;
    Ok((run, turn))
}

fn actor(caller: Option<&Origin>) -> Value {
    caller.map(|value|json!({"kind":"shell","thread_id":value.thread_id,"run_id":value.run_id,"execution_id":value.execution_id,"turn_id":value.turn_id})).unwrap_or_else(||json!({"kind":"human"}))
}

fn queue_compact_tx(
    tx: &Transaction<'_>,
    thread: Option<&str>,
    caller: Option<&Origin>,
    operation_id: &str,
    edit: Option<&ContextEditArgs>,
) -> Result<MutationResult> {
    let thread = target(thread, caller)?;
    thread_details(tx, &thread)?;
    let key = crate::compaction::CompactionKeys::new(&thread).pending();
    let pending: Option<String> = tx
        .query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |r| {
            r.get(0)
        })
        .optional()
        .map_err(sql_error)?;
    let mut operations: Vec<String> = match pending {
        Some(body) => serde_json::from_str(&body).map_err(|_| {
            error(
                ErrorCode::InvalidState,
                "Pending compaction metadata is malformed",
            )
        })?,
        None => Vec::new(),
    };
    operations.push(operation_id.into());
    tx.execute("INSERT INTO app_meta(key,value)VALUES(?1,?2)ON CONFLICT(key)DO UPDATE SET value=excluded.value",params![key,serde_json::to_string(&operations).unwrap()]).map_err(sql_error)?;
    let mut result = json!({"thread_id":thread,"status":"queued","operation_id":operation_id});
    if let Some(edit) = edit {
        let mut edit = edit.clone();
        edit.thread_id = Some(thread.clone());
        result["context_edit"] = serde_json::to_value(edit)
            .map_err(|_| error(ErrorCode::InvalidState, "Cannot encode context edit"))?;
    }
    Ok(MutationResult {
        result,
        compact_threads: vec![thread],
        ..Default::default()
    })
}

fn target(id: Option<&str>, caller: Option<&Origin>) -> Result<String> {
    id.map(str::to_owned)
        .or_else(|| caller.map(|caller| caller.thread_id.clone()))
        .ok_or_else(|| {
            error(
                ErrorCode::InvalidArgument,
                "external operations must specify --thread",
            )
        })
}

pub fn thread_details(connection: &Connection, id: &str) -> Result<ThreadDetails> {
    let mut details=connection.query_row("SELECT title,cwd,profile_id,model_id,effort,parent_thread_id,agents_path,auto_compact_at,segment_chars,reduce_rounds,archived_at,auto_compact_target_percent,summary_max_chars FROM thread WHERE id=?1 AND deleted_at IS NULL",[id],|row|Ok(ThreadDetails { instruction_sources: None, archived_at: row.get(10)?, tags: Vec::new(), auto_compact_at: row.get::<_,Option<i64>>(7)?.map(|v|v as u64), effective_auto_compact_at: None, auto_compact_target_percent: row.get::<_,Option<i64>>(11)?.map(|v|v as u64), summary_max_chars:row.get::<_,Option<i64>>(12)?.map(|v|v as u64), effective_auto_compact_target_percent:None,effective_summary_max_chars:None,segment_chars: row.get::<_,Option<i64>>(8)?.map(|v|v as u64), effective_segment_chars: None, reduce_rounds: row.get::<_,Option<i64>>(9)?.map(|v|v as u64), effective_reduce_rounds: None, awaiting_replies: Vec::new(), last_end_message: None, last_input: None,  last_compaction:None,usage_totals:None, agents_path: row.get(6)?,context_usage:None,thread_id:id.into(),title:row.get(0)?,cwd:row.get(1)?,connection:row.get(2)?,model:row.get(3)?,effort:row.get(4)?,parent_id:row.get(5)?,run_id:None,turn_id:None,shells_count:0,recovery_reason:None,parent_change:None,context:None})).optional().map_err(sql_error)?.ok_or_else(||error(ErrorCode::ThreadNotFound,"Thread not found"))?;
    details.instruction_sources = connection.query_row(
        "SELECT CASE WHEN s.content_hash GLOB 'instruction-bundle-v1:sha256:*' AND json_valid(substr(s.body,38)) THEN json_extract(substr(s.body,38),'$.extra') ELSE NULL END FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.thread_id=?1 ORDER BY t.created_at DESC,t.rowid DESC LIMIT 1",[id],|r|r.get::<_,Option<String>>(0)).optional().map_err(sql_error)?.flatten().and_then(|body|serde_json::from_str::<Vec<Value>>(&body).ok()).map(|mut files| {for file in &mut files {if let Some(file)=file.as_object_mut() { file.remove("body"); }} json!({"extra":files,"snapshot":"latest_prepared_turn"})});
    details.tags = tags::for_thread(connection, id)?;
    details.context = Some(crate::context::for_thread(connection, id)?);
    details.last_end_message = crate::thread_observation::last_end_message(connection, id)?;
    details.last_input = crate::thread_observation::last_input(connection, id)?;
    details.awaiting_replies =
        super::reply::awaiting_replies_tx(connection, id, None).map_err(sql_error)?;
    details.title = thread_display_title(connection, id, &details.title)?;
    details.run_id = connection
        .query_row(
            "SELECT id FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    if let Some(run) = &details.run_id {
        let turn:Option<(String,Option<String>)>=connection.query_row("SELECT id,recovery_reason_json FROM turn WHERE run_id=?1 AND state IN ('preparing','running','waiting','recovery_wait')",[run],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(sql_error)?;
        if let Some((id, reason)) = turn {
            details.recovery_reason = recovery_status(connection, &details.thread_id, &id, reason)?;
            details.turn_id = Some(id);
        }

        details.shells_count = count(
            connection,
            "SELECT count(*) FROM shell_execution WHERE owner_run_id=?1 AND state IN ('starting','running','stopping')",
            run,
        )?;
    }
    Ok(details)
}

fn title_text(text: &str) -> String {
    crate::presentation::sanitize_plain(&text.replace('\r', "\n"), 0)
        .0
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn bounded_thread_title(text: &str) -> String {
    let normalized = title_text(text);
    if normalized.graphemes(true).count() <= 80 && normalized.chars().count() <= 160 {
        return normalized;
    }
    let mut title = String::new();
    let mut chars = 0;
    for grapheme in normalized.graphemes(true).take(79) {
        let count = grapheme.chars().count();
        if chars + count + 1 > 160 {
            break;
        }
        title.push_str(grapheme);
        chars += count;
    }
    title.push('…');
    title
}

/// Pure display policy shared with the frontend's off-worker title preparation.
pub(crate) fn title_from_user_payload(payload: &Value) -> Option<String> {
    if payload.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let text = match payload.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(content)) => content
            .iter()
            .filter(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("input_text" | "text")
                )
            })
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let title = bounded_thread_title(&text);
    (!title.is_empty()).then_some(title)
}

/// Display-only fallback. Never rewrite an explicit title or the user's history.
/// The selected path, rather than a Run's latest assistant/tool output, defines
/// the earliest meaningful user text, including on a forked/shared history.
pub(crate) fn thread_display_title(
    connection: &Connection,
    id: &str,
    stored: &str,
) -> Result<String> {
    if !stored.trim().is_empty() {
        return Ok(stored.into());
    }
    let sources = [
        "SELECT n.payload_json FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind='user' ORDER BY p.ordinal",
        // Durable acceptance precedes application to thread_path. Only pending
        // receipts qualify; an applied message outside the chosen path must not
        // resurrect history that a fork/rewind deliberately excluded.
        "SELECT n.payload_json FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=?1 AND r.state IN ('accepted','held') AND n.kind='user' AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?1 AND p.node_id=n.id) ORDER BY r.created_at,n.rowid",
    ];
    let mut user_exists = false;
    for source in sources {
        let mut statement = connection.prepare(source).map_err(sql_error)?;
        let mut rows = statement.query([id]).map_err(sql_error)?;
        while let Some(row) = rows.next().map_err(sql_error)? {
            user_exists = true;
            let body: Option<String> = row.get(0).map_err(sql_error)?;
            let Some(payload) = body.and_then(|body| serde_json::from_str::<Value>(&body).ok())
            else {
                continue;
            };
            if let Some(title) = title_from_user_payload(&payload) {
                return Ok(title);
            }
        }
    }
    Ok(if user_exists { "Untitled" } else { "Unsent" }.into())
}

fn thread_excerpt(connection: &Connection, thread: &str) -> Result<String> {
    let mut statement=connection.prepare("SELECT n.payload_json FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.payload_json IS NOT NULL AND n.kind IN ('user','user_message','message','assistant','assistant_response') ORDER BY p.ordinal DESC").map_err(sql_error)?;
    let mut rows = statement.query([thread]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let body: String = row.get(0).map_err(sql_error)?;
        let Ok(payload) = serde_json::from_str::<Value>(&body) else {
            continue;
        };
        let excerpt = history::conversation_excerpt(&payload, 512);
        if !excerpt.trim().is_empty() {
            return Ok(excerpt);
        }
    }
    Ok(String::new())
}

/// A cursor never crosses archived modes: each mode has its own scope string.
fn list_scope(mode: ArchivedFilter) -> &'static str {
    match mode {
        ArchivedFilter::Exclude => "thread.list",
        ArchivedFilter::Include => "thread.list.include_archived",
        ArchivedFilter::Only => "thread.list.only_archived",
    }
}

pub(crate) fn thread_list(
    connection: &Connection,
    args: &ThreadListArgs,
) -> Result<Page<ThreadSummary>> {
    if args.awaiting.as_deref() == Some("human") {
        return thread_list_awaiting_human(connection, args);
    }
    let filter = args.query.as_deref().unwrap_or("");
    let needle = title_text(filter).to_lowercase();
    // Tags and cwd narrow in SQL; the cursor is bound to them through a digest.
    let tags = tags::normalize_all(&args.tags)?;
    // Same lexical rule as saved cwds (`normalize`): `/work/proj/` and `/work/./proj` find
    // `/work/proj`; a relative path has no base here and is refused, never silently empty.
    let cwd_normalized = match args.cwd.as_deref() {
        None => None,
        Some(raw) if Path::new(raw).is_absolute() => Some(
            normalize(Path::new(raw), Path::new("/"))?
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| error(ErrorCode::InvalidArgument, "cwd must be valid UTF-8"))?,
        ),
        Some(_) => {
            return Err(error(
                ErrorCode::InvalidArgument,
                "cwd filter must be an absolute path",
            ));
        }
    };
    let cwd = cwd_normalized.as_deref();
    let cursor_filter = list_cursor_filter(filter, &tags, cwd);
    let watermark: i64 = connection
        .query_row("SELECT COALESCE(MAX(rowid),0) FROM thread", [], |row| {
            row.get(0)
        })
        .map_err(sql_error)?;
    let (key, watermark) = if let Some(token) = &args.cursor {
        let cursor = PageCursor::decode(token, list_scope(args.archived), &cursor_filter, 0, None)?;
        (
            Some(cursor.last_sort_key),
            i64::try_from(cursor.watermark)
                .map_err(|_| error(ErrorCode::CursorExpired, "invalid thread watermark"))?,
        )
    } else {
        (None, watermark)
    };
    let (last_time, last_id) = if let Some(key) = key {
        serde_json::from_str::<(String, String)>(&key)
            .map_err(|_| error(ErrorCode::CursorExpired, "invalid thread list position"))?
    } else {
        (String::new(), String::new())
    };
    // Filter against the same derived labels returned by ThreadGet/List. Keep
    // candidates as a cursor: a filtered miss scans without collecting history.
    let mut statement = connection
        .prepare(&thread_summary::page_sql(
            args.archived,
            tags.len(),
            cwd.is_some(),
        ))
        .map_err(sql_error)?;
    let page_limit = if needle.is_empty() {
        i64::from(args.limit) + 1
    } else {
        -1
    };
    let mut binds: Vec<&dyn rusqlite::ToSql> = vec![&watermark, &last_time, &last_id, &page_limit];
    binds.extend(tags.iter().map(|tag| tag as &dyn rusqlite::ToSql));
    binds.extend(cwd.iter().map(|path| path as &dyn rusqlite::ToSql));
    let mut rows = statement.query(binds.as_slice()).map_err(sql_error)?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let raw = thread_summary::decode(row).map_err(sql_error)?;
        let id = &raw.thread_id;
        let title = thread_display_title(connection, id, &raw.stored_title)?;
        if !thread_summary::matches_filter(id, &title, &raw.cwd, filter) {
            continue;
        }
        let excerpt = thread_excerpt(connection, id)?;
        let mut summary = thread_summary::prepare(raw, title, excerpt);
        summary.last_end_message =
            crate::thread_observation::last_end_message(connection, &summary.thread_id)?;
        summary.last_input = crate::thread_observation::last_input(connection, &summary.thread_id)?;
        items.push(summary);
        if items.len() > args.limit as usize {
            break;
        }
    }
    let next_cursor = if items.len() > args.limit as usize {
        items.pop();
        let last = items.last().unwrap();
        Some(
            PageCursor {
                version: 1,
                scope: list_scope(args.archived).into(),
                filter: cursor_filter.clone(),
                last_sort_key: serde_json::to_string(&(
                    last.updated_at.clone(),
                    last.thread_id.clone(),
                ))
                .unwrap(),
                watermark: watermark as u64,
                revision: 0,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    // One statement for the whole page: bounded by the page, not by the stored Threads.
    let ids: Vec<String> = items.iter().map(|item| item.thread_id.clone()).collect();
    let mut tagged = tags::for_threads(connection, &ids)?;
    for item in &mut items {
        item.tags = tagged.remove(&item.thread_id).unwrap_or_default();
    }
    Ok(Page { items, next_cursor })
}

/// Enumerate only target Threads present in the partial pending-reply index.
/// The dedicated partial index excludes Thread-directed replies before paging,
/// so work follows human reply duties rather than the complete Thread table.
fn thread_list_awaiting_human(
    connection: &Connection,
    args: &ThreadListArgs,
) -> Result<Page<ThreadSummary>> {
    const SCOPE: &str = "thread.list.awaiting_human.live.v2";
    let archived_filter = format!("human:{}", args.archived.as_str());
    let decoded = args
        .cursor
        .as_ref()
        .map(|token| PageCursor::decode_live(token, SCOPE, &archived_filter, 0, None))
        .transpose()?;
    let after = decoded
        .as_ref()
        .map_or("", |cursor| cursor.last_sort_key.as_str());
    let candidates = awaiting_human_candidates(connection, after, args.limit)?;
    let has_more = candidates.len() > args.limit as usize;
    let examined = &candidates[..candidates.len().min(args.limit as usize)];
    let candidate_ids: Vec<String> = examined.iter().map(|(id, _)| id.clone()).collect();
    let mut visible_ids = Vec::with_capacity(candidate_ids.len());
    for id in &candidate_ids {
        let row: Option<Option<String>> = connection
            .query_row(
                "SELECT archived_at FROM thread WHERE id=?1 AND deleted_at IS NULL",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        let Some(archived_at) = row else {
            continue;
        };
        if matches!(
            (args.archived, archived_at.is_some()),
            (ArchivedFilter::Exclude, true) | (ArchivedFilter::Only, false)
        ) {
            continue;
        }
        visible_ids.push(id.clone());
    }
    let raw = thread_summary::read_ids_tx(connection, &visible_ids).map_err(storage_error)?;
    let mut items = Vec::with_capacity(raw.len());
    for raw in raw {
        let id = raw.thread_id.clone();
        let title = thread_display_title(connection, &id, &raw.stored_title)?;
        let excerpt = thread_excerpt(connection, &id)?;
        let mut item = thread_summary::prepare(raw, title, excerpt);
        item.last_end_message = crate::thread_observation::last_end_message(connection, &id)?;
        item.last_input = crate::thread_observation::last_input(connection, &id)?;
        items.push(item);
    }
    for item in &mut items {
        if let Some((_, counts)) = candidates.iter().find(|(id, _)| id == &item.thread_id) {
            item.awaiting_human_count = counts.count;
            item.awaiting_human_count_exact = counts.exact;
            item.awaiting_human_oldest_at = counts.oldest.clone();
        }
    }
    items.retain(|item| item.awaiting_human_count > 0);
    let mut tagged = tags::for_threads(connection, &visible_ids)?;
    for item in &mut items {
        item.tags = tagged.remove(&item.thread_id).unwrap_or_default();
    }
    let next_cursor = if has_more {
        let last = examined.last().expect("continued page has candidates");
        Some(
            PageCursor {
                version: 2,
                scope: SCOPE.into(),
                filter: archived_filter,
                last_sort_key: last.0.clone(),
                watermark: 0,
                revision: 0,
                encoding: None,
            }
            .encode()
            .map_err(|_| error(ErrorCode::InvalidState, "cannot encode thread cursor"))?,
        )
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

#[derive(Debug)]
struct AwaitingHumanCount {
    count: u64,
    exact: bool,
    oldest: Option<String>,
}

/// Read a bounded page of pending Thread keys and exact page-local aggregates.
fn awaiting_human_candidates(
    connection: &Connection,
    after: &str,
    limit: u32,
) -> Result<Vec<(String, AwaitingHumanCount)>> {
    let mut statement = connection
        .prepare(awaiting_human_candidates_sql())
        .map_err(sql_error)?;
    let rows = statement
        .query_map(
            params![
                after,
                i64::from(limit) + 1,
                (crate::protocol::MAX_HUMAN_REPLY_SUMMARY_DUTIES + 1) as i64
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(sql_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map(|rows| {
            rows.into_iter()
                .map(|(id, count, oldest)| {
                    let exact =
                        count < (crate::protocol::MAX_HUMAN_REPLY_SUMMARY_DUTIES + 1) as i64;
                    (
                        id,
                        AwaitingHumanCount {
                            count: count.max(0) as u64,
                            exact,
                            oldest: if exact { oldest } else { None },
                        },
                    )
                })
                .collect()
        })
        .map_err(sql_error)
}

/// Each candidate costs at most one index seek; per-Thread aggregates stop at 17.
fn awaiting_human_candidates_sql() -> &'static str {
    "WITH RECURSIVE page(target_thread_id) AS (\
       SELECT (SELECT r.target_thread_id FROM receipt r INDEXED BY receipt_human_reply_pending \
         WHERE r.target_thread_id>?1 AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.result_json,'$.to_human')=1 ORDER BY r.target_thread_id LIMIT 1) \
       UNION ALL SELECT (SELECT r.target_thread_id FROM receipt r INDEXED BY receipt_human_reply_pending \
         WHERE r.target_thread_id>page.target_thread_id AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.result_json,'$.to_human')=1 ORDER BY r.target_thread_id LIMIT 1) FROM page WHERE page.target_thread_id IS NOT NULL LIMIT ?2) \
     SELECT page.target_thread_id,\
       (SELECT count(*) FROM (SELECT 1 FROM receipt r INDEXED BY receipt_human_reply_pending WHERE r.target_thread_id=page.target_thread_id \
         AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.result_json,'$.to_human')=1 LIMIT ?3)),\
       CASE WHEN (SELECT count(*) FROM (SELECT 1 FROM receipt r INDEXED BY receipt_human_reply_pending WHERE r.target_thread_id=page.target_thread_id \
         AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.result_json,'$.to_human')=1 LIMIT ?3))<?3 \
         THEN (SELECT min(r.created_at) FROM (SELECT r.created_at FROM receipt r INDEXED BY receipt_human_reply_pending WHERE r.target_thread_id=page.target_thread_id \
         AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.result_json,'$.to_human')=1 ORDER BY r.created_at LIMIT ?3) r) END \
     FROM page WHERE page.target_thread_id IS NOT NULL ORDER BY page.target_thread_id"
}

/// The cursor's filter: the text query alone when nothing else narrows the list,
/// otherwise the query plus a digest of the sorted tags and the cwd.
fn list_cursor_filter(query: &str, tags: &[String], cwd: Option<&str>) -> String {
    if tags.is_empty() && cwd.is_none() {
        return query.to_owned();
    }
    let mut sorted = tags.to_vec();
    sorted.sort();
    format!(
        "{query}\u{0}{}",
        hash(json!({"tags": sorted, "cwd": cwd}).to_string().as_bytes())
    )
}

pub fn message_details(connection: &Connection, id: &str) -> Result<MessageDetails> {
    let row=connection.query_row("SELECT r.target_thread_id,r.state,r.run_id,r.turn_id,r.result_json,NULL,r.sender_json FROM receipt r LEFT JOIN turn t ON t.id=r.turn_id WHERE message_node_id=?1",[id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,String>(6)?))).optional().map_err(sql_error)?.ok_or_else(||error(ErrorCode::TargetUnavailable,"message not found"))?;
    let status = match row.1.as_str() {
        "accepted" => DeliveryStatus::Accepted,
        "held" => DeliveryStatus::Held,
        "applied" => DeliveryStatus::Applied,
        "cancelled" => DeliveryStatus::Cancelled,
        _ => {
            return Err(error(
                ErrorCode::InvalidState,
                "unknown saved delivery state",
            ));
        }
    };
    // Internal receipt metadata also contains current cancellation facts; it is
    // not a second public MessageAccepted wire response.
    #[derive(Deserialize)]
    struct SavedAssignment {
        assignment: Assignment,
    }
    let metadata: Value = row
        .4
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!({}));
    let saved: Option<SavedAssignment> = row.4.and_then(|json| serde_json::from_str(&json).ok());
    let assignment = if row.2.is_none() {
        Assignment::Pending
    } else {
        saved
            .map(|saved| saved.assignment)
            .filter(|assignment| *assignment != Assignment::Pending)
            .unwrap_or(Assignment::ExistingTurn)
    };
    Ok(MessageDetails {
        to_human: metadata["to_human"] == true,
        sender: MessageSender::from_saved(Some(&row.6)),
        context: crate::context::for_message(connection, id)?,
        message_id: id.into(),
        thread_id: row.0,
        delivery_status: status,
        assignment,
        run_id: row.2,
        turn_id: row.3,
        expect_reply: metadata["expect_reply"] == true,
        reply: metadata.get("reply").cloned(),
        reply_to: metadata["reply_to"].as_str().map(str::to_owned),
    })
}

pub(crate) fn saved_usage_for_detail(
    db: &Connection,
    scope: &crate::usage_totals::Scope,
    context: crate::usage_totals::ReadContext,
    diagnostics: Option<&crate::diagnostics::Diagnostics>,
) -> Result<Value> {
    match crate::usage_totals::read_logged(db, scope, context, diagnostics) {
        Ok(value) => Ok(value),
        Err(e)
            if matches!(
                e.code,
                ErrorCode::TargetUnavailable | ErrorCode::ThreadNotFound
            ) =>
        {
            Ok(json!({"available":false,"reason":"scope_deleted_or_unavailable"}))
        }
        Err(e) => Err(e),
    }
}

pub fn run_details(connection: &Connection, id: &str) -> Result<RunDetails> {
    let row = connection
        .query_row("SELECT thread_id,state FROM run WHERE id=?1", [id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| error(ErrorCode::TargetUnavailable, "Run not found"))?;
    let state = serde_json::from_value(json!(row.1))
        .map_err(|_| error(ErrorCode::InvalidState, "unknown saved Run state"))?;
    let turn:Option<(String,Option<String>,Option<String>)>=connection.query_row("SELECT id,answer_node_id,recovery_reason_json FROM turn WHERE run_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(sql_error)?;
    let (turn_id, end_message_node_id, recovery_hint) = turn
        .map_or((None, None, None), |(id, answer, reason)| {
            (Some(id), answer, reason)
        });
    let recovery_hint = match turn_id.as_deref() {
        Some(turn) => recovery_status(connection, &row.0, turn, recovery_hint)?,
        None => recovery_hint,
    };
    Ok(RunDetails {
        usage_totals: None,
        waiting_reason: crate::readback::run_wait_reason(connection, &row.0, id)?,
        revisit: super::revisit::summary_for_run(connection, id).map_err(sql_error)?,
        context: crate::context::for_run(connection, id)?,
        run_id: id.into(),
        thread_id: row.0,
        state,
        turn_id,
        end_message_node_id,
        recovery_hint,
        owned_execution_ids: ids(
            connection,
            "SELECT id FROM shell_execution WHERE owner_run_id=?1 AND state IN ('starting','running','stopping') ORDER BY started_at,id",
            id,
        )?,
    })
}

// Status readback, not reply admission: retain the existing recovery field and
// add the exact held count so each committed recovery unit is observable.
fn recovery_status(
    connection: &Connection,
    thread: &str,
    turn: &str,
    reason: Option<String>,
) -> Result<Option<String>> {
    let recovering: bool = connection
        .query_row(
            "SELECT state='recovery_wait' FROM turn WHERE id=?1",
            [turn],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if !recovering {
        return Ok(reason);
    }
    let remaining: i64 = connection.query_row("SELECT count(*) FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='held'", [thread], |r| r.get(0)).map_err(sql_error)?;
    if remaining == 0 {
        return Ok(reason);
    }
    let mut value = reason
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"reason":reason}));
    value["remaining_held"] = json!(remaining);
    Ok(Some(value.to_string()))
}

pub fn turn_details(connection: &Connection, id: &str) -> Result<TurnDetails> {
    connection.query_row("SELECT run_id,thread_id,state,answer_node_id,recovery_reason_json FROM turn WHERE id=?1",[id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,Option<String>>(4)?))).optional().map_err(sql_error)?.ok_or_else(||error(ErrorCode::TargetUnavailable,"Turn not found")).and_then(|row|Ok(TurnDetails{usage_totals:None,context:crate::context::for_turn(connection,id)?,turn_id:id.into(),run_id:row.0,thread_id:row.1,state:serde_json::from_value(json!(row.2)).map_err(|_|error(ErrorCode::InvalidState,"unknown saved Turn state"))?,end_message_node_id:row.3,recovery_hint:row.4}))
}

fn stop_run(tx: &Transaction<'_>, id: &str, now: &str) -> Result<MutationResult> {
    let details = run_details(tx, id)?;
    let mut result = MutationResult::default();
    if matches!(details.state, RunState::Live | RunState::Stopping) {
        result.stop_runs = vec![id.to_owned()];
        for run in &result.stop_runs {
            tx.execute("UPDATE run SET state='stopping',stop_reason='explicit_stop' WHERE id=?1 AND state='live'",[run]).map_err(sql_error)?;
            tx.execute("UPDATE turn SET generation=generation+1 WHERE run_id=?1 AND state IN ('preparing','running','waiting','recovery_wait')",[run]).map_err(sql_error)?;
            // The stopping state is visible now; do not wait for reply_grace (#243).
            let thread: String = tx
                .query_row("SELECT thread_id FROM run WHERE id=?1", [run], |r| r.get(0))
                .map_err(sql_error)?;
            if !result.changed_threads.contains(&thread) {
                result.changed_threads.push(thread);
            }
            super::attachment_delivery::hold_run_tx(tx, run)?;
            tx.execute("UPDATE receipt SET state='held',hold_reason='stopping',updated_at=?2 WHERE run_id=?1 AND state='accepted'",params![run,now]).map_err(sql_error)?;
        }
    }
    result.result = serde_json::to_value(run_details(tx, id)?).unwrap();
    Ok(result)
}

fn retry_run(
    tx: &Transaction<'_>,
    id: &str,
    caller: Option<&Origin>,
    now: &str,
) -> Result<MutationResult> {
    let details = run_details(tx, id)?;
    if details.state != RunState::Live {
        return Err(error(
            ErrorCode::InvalidState,
            "only a live Run in recovery_wait can retry",
        ));
    }
    let recovery: Option<(String, i64)> = tx
        .query_row(
            "SELECT id,generation FROM turn WHERE run_id=?1 AND state='recovery_wait'",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let Some((turn, generation)) = recovery else {
        return Err(error(
            ErrorCode::InvalidState,
            "Run has no recoverable Turn",
        ));
    };
    if !super::state::resume_recovery_turn_tx(tx, &details.thread_id, id, &turn, generation, now)
        .map_err(storage_error)?
    {
        return Err(error(
            ErrorCode::InvalidState,
            "Run has no recoverable Turn",
        ));
    }
    super::context::record_fact(
        tx,
        &details.thread_id,
        "recovery_resumed",
        actor(caller),
        json!({"run_id":id}),
        now,
        super::notification::Fact::RecoveryResumed,
    )
    .map_err(sql_error)?;
    Ok(MutationResult {
        committed_error: None,
        owner_shutdown: false,
        result: serde_json::to_value(run_details(tx, id)?).unwrap(),
        wake_threads: vec![],
        changed_threads: vec![],
        quiet_wake_threads: vec![details.thread_id],
        stop_runs: vec![],
        compact_threads: vec![],
    })
}

fn delete_thread(
    tx: &Transaction<'_>,
    args: &ThreadDeleteArgs,
    now: &str,
) -> Result<MutationResult> {
    if args.preview_marker.is_none() {
        thread_details(tx, &args.thread_id)?;
    }
    let targets = if let Some(expected) = &args.preview_marker {
        let targets = crate::deletion::guarded_targets(tx, args)?.ok_or_else(|| {
            error(
                ErrorCode::PreviewStale,
                "deletion preview is stale or exceeds its target limit; request a new preview",
            )
        })?;
        if crate::deletion::target_marker(targets.iter().map(String::as_str)) != *expected {
            return Err(error(
                ErrorCode::PreviewStale,
                "deletion preview target set changed; request a new preview",
            ));
        }
        targets
    } else if args.descendants {
        let mut statement=tx.prepare("WITH RECURSIVE children(id) AS(SELECT ?1 UNION SELECT t.id FROM thread t JOIN children c ON t.parent_thread_id=c.id WHERE t.deleted_at IS NULL) SELECT id FROM children").map_err(sql_error)?;
        statement
            .query_map([&args.thread_id], |row| row.get::<_, String>(0))
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?
    } else {
        vec![args.thread_id.clone()]
    };
    if args.preview_marker.is_some() {
        thread_details(tx, &args.thread_id)?;
    }
    for id in &targets {
        if count(
            tx,
            "SELECT count(*) FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",
            id,
        )? > 0
        {
            return Err(error(
                ErrorCode::InvalidState,
                "stop every selected Thread before deleting it",
            ));
        }
    }
    for id in &targets {
        tx.execute("UPDATE thread SET deleted_at=?2,revision=revision+1,selection_revision=selection_revision+1,cursor_node_id=NULL,updated_at=?2 WHERE id=?1",params![id,now]).map_err(sql_error)?;
        crate::compaction::a_state::cleanup_tx(tx,id).map_err(storage_error)?;
        // Retain the last path/replacement as ownership evidence until payload
        // GC. Deleted Threads are not roots or public selection targets. Removing
        // these rows now makes turn-less summaries/legacy nodes look ownerless.
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [crate::index::IndexKeys::observed(id)],
        )
        .map_err(sql_error)?;
        tx.execute(
            "DELETE FROM app_meta WHERE key IN (?1,?2,?3,?4,?5)",
            params![
                crate::compaction::CompactionKeys::new(id).automatic_target(),
                // #226: a stopped Thread's stale automatic-active witness.
                crate::compaction::CompactionKeys::new(id).automatic_active(),
                crate::compaction::CompactionKeys::new(id).automatic_guard(),
                crate::compaction::CompactionKeys::new(id).automatic_status(),
                // #182: the kept-reasoning set ends with its Thread.
                history::reasoning_keep_key(id)
            ],
        )
        .map_err(sql_error)?;
        // Tags end with their Thread (#222): a deleted Thread leaves no tag name behind.
        tx.execute("DELETE FROM thread_tag WHERE thread_id=?1", [id])
            .map_err(sql_error)?;
        tx.execute("UPDATE attachment_delivery SET status='cancelled' WHERE thread_id=?1 AND status IN ('accepted','held')",[id]).map_err(sql_error)?;
        tx.execute("UPDATE receipt SET state='cancelled',hold_reason='target_unavailable',updated_at=?2 WHERE target_thread_id=?1 AND state IN ('accepted','held')",params![id,now]).map_err(sql_error)?;
    }
    let reply_wakes = super::reply::deleted_threads_tx(tx, &targets, now).map_err(sql_error)?;
    bump_roots(tx)?;
    let revision: i64 = tx
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    tx.execute("INSERT INTO gc_job(id,kind,root_revision,cursor_json,state) VALUES(?1,'history',?2,?3,'pending')",params![uuid::Uuid::new_v4().to_string(),revision,json!({"deleted_thread_ids":targets}).to_string()]).map_err(sql_error)?;
    let mut result = MutationResult::value(Accepted { accepted: true })?;
    // The caller publishes these existing notifications only after commit.
    // A receipt replay returns its saved value without repeating the effects.
    result.changed_threads = targets;
    result.quiet_wake_threads = reply_wakes;
    Ok(result)
}

fn bump_roots(tx: &Transaction<'_>) -> Result<()> {
    if tx
        .execute(
            "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
            [],
        )
        .map_err(sql_error)?
        != 1
    {
        return Err(error(
            ErrorCode::SaveFailed,
            "root revision metadata missing",
        ));
    }
    Ok(())
}
fn normalize(path: &Path, base: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    if !result.is_absolute() {
        return Err(error(
            ErrorCode::InvalidArgument,
            "cwd must resolve to an absolute path",
        ));
    }
    Ok(result)
}
fn count(connection: &Connection, sql: &str, id: &str) -> Result<u64> {
    connection
        .query_row(sql, [id], |row| row.get::<_, i64>(0))
        .map(|value| value as u64)
        .map_err(sql_error)
}
fn ids(connection: &Connection, sql: &str, id: &str) -> Result<Vec<String>> {
    connection
        .prepare(sql)
        .map_err(sql_error)?
        .query_map([id], |row| row.get(0))
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)
}
fn error(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError::new(code, message)
}
fn sql_error(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("database operation failed: {error}"))
}
fn storage_error(error: StorageError) -> ApiError {
    error.into_api_error()
}

/// Receipt completion describes the command, not the asynchronous work it queued.
pub(crate) fn operation_details(
    db: &Connection,
    id: &str,
    current_instance: &str,
) -> Result<Value> {
    let row: Option<(String, String, String, Option<String>)> = db.query_row(
        "SELECT instance_id,created_at,state,result_ref_json FROM operation_receipt WHERE operation_id=?1",
        [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).optional().map_err(sql_error)?;
    let Some((instance, created_at, state, body)) = row else {
        return Err(error(
            ErrorCode::OperationNotFound,
            "operation receipt not found",
        ));
    };
    let (outcome, expired) = super::receipt::decode_operation_outcome(body.as_deref())?;
    let result = outcome.as_ref().and_then(|v| v.get("result"));
    let mut value = super::receipt::operation_envelope(
        id,
        &instance,
        &created_at,
        &state,
        &outcome,
        expired,
        current_instance,
    );
    if result.is_some_and(|v| {
        v.get("status").and_then(Value::as_str) == Some("queued")
            && v.get("operation_id").and_then(Value::as_str) == Some(id)
    }) {
        let saved: Option<String> = db
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [crate::compaction::CompactionKeys::result(id)],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        if let Some(saved) = saved {
            value["downstream"] = serde_json::from_str(&saved)
                .map_err(|_| error(ErrorCode::InvalidState, "compaction outcome is malformed"))?;
        } else if let Some(thread) = result
            .and_then(|v| v.get("thread_id"))
            .and_then(Value::as_str)
        {
            let mut status = "unknown";
            for (key, candidate) in [
                (
                    crate::compaction::CompactionKeys::new(thread).pending(),
                    "queued",
                ),
                (
                    crate::compaction::CompactionKeys::new(thread).inflight(),
                    "running",
                ),
            ] {
                let body: Option<String> = db
                    .query_row("SELECT value FROM app_meta WHERE key=?1", [key], |r| {
                        r.get(0)
                    })
                    .optional()
                    .map_err(sql_error)?;
                if body
                    .and_then(|b| serde_json::from_str::<Vec<String>>(&b).ok())
                    .is_some_and(|ids| ids.iter().any(|op| op == id))
                {
                    status = candidate;
                }
            }
            value["downstream"] = json!({"kind":"compact", "state":status});
        }
    } else if let Some(message) = result
        .and_then(|v| v.get("message_id"))
        .and_then(Value::as_str)
    {
        let delivery: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT state,hold_reason FROM receipt WHERE message_node_id=?1",
                [message],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        if let Some((state, reason)) = delivery {
            let mut downstream = json!({"kind":"message", "state":state});
            if let Some(reason) = reason {
                downstream["hold_reason"] = json!(reason);
            }
            value["downstream"] = downstream;
        }
    }
    Ok(value)
}

/// What the saved request hash covers. A stored key is replaced by a digest salted with
/// a per-process random value, so the receipt table never holds a hash that an offline
/// guess of the key could confirm. The salt only needs to outlive the owner instance.
fn fingerprint_body(command: &Command, caller: Option<&Origin>) -> Value {
    static SALT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    match command {
        Command::CredentialsSet(args) => {
            let salt = SALT.get_or_init(|| uuid::Uuid::new_v4().to_string());
            json!({
                "command": {"credentials.set": {
                    "connection": args.connection,
                    "value_digest": hash(format!("{salt}\0{}", args.value)),
                }},
                "caller": caller,
            })
        }
        _ => json!({"command":command,"caller":caller}),
    }
}
fn request_fingerprint<'a>(
    cached: &'a std::sync::OnceLock<Result<String>>,
    request: &Request,
    caller: Option<&Origin>,
) -> Result<&'a str> {
    cached
        .get_or_init(|| {
            serde_json::to_vec(&fingerprint_body(&request.command, caller))
                .map(hash)
                .map_err(|_| error(ErrorCode::InvalidArgument, "invalid request encoding"))
        })
        .as_ref()
        .map(String::as_str)
        .map_err(Clone::clone)
}

fn saved_operation(
    db: &Connection,
    operation_id: &str,
    fingerprint: &str,
) -> Result<Option<MutationResult>> {
    let saved: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT request_hash,result_ref_json FROM operation_receipt WHERE operation_id=?1",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    if let Some((saved_hash, body)) = saved {
        if saved_hash != fingerprint {
            return Err(error(
                ErrorCode::OperationConflict,
                "operation ID was already used with different arguments",
            ));
        }
        let body = body.ok_or_else(|| {
            error(
                ErrorCode::InvalidState,
                "saved operation has no committed result",
            )
        })?;
        if body == super::receipt::EXPIRED_OUTCOME {
            return Err(super::receipt::expired_error());
        }
        let outcome: SavedOutcome = serde_json::from_str(&body).map_err(|_| {
            error(
                ErrorCode::InvalidState,
                "saved operation result is malformed",
            )
        })?;
        return match (outcome.result, outcome.error) {
            (Some(result), None) => Ok(Some(MutationResult {
                result,
                ..MutationResult::default()
            })),
            (None, Some(error)) => Err(error),
            _ => Err(error(
                ErrorCode::InvalidState,
                "saved operation result is inconsistent",
            )),
        };
    }
    Ok(None)
}

#[cfg(test)]
mod thread_list_plan_tests {
    use super::*;

    #[test]
    fn awaiting_human_late_arrivals_do_not_increase_page_vm_steps_and_live_cursor_recovers() {
        fn setup(late: usize) -> Connection {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('a','','','p','m','now'),('b','','','p','m','now'),('z','','','p','m','now'); INSERT INTO node(id,kind,created_at,payload_json) VALUES('na','message','1','{}'),('nb','message','2','{}'),('nz','message','3','{}');").unwrap();
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES('ra','oa','{}','a','na','applied','10','{\"reply\":{\"state\":\"pending\"},\"to_human\":true}')",[]).unwrap();
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES('rb','ob','{}','b','nb','applied','11','{\"reply\":{\"state\":\"pending\"},\"to_human\":true}')",[]).unwrap();
            let tx = db.unchecked_transaction().unwrap();
            for n in 0..late {
                let id = format!("m{n:06}");
                let node = format!("n{n:06}");
                tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,'','','p','m','now')",[&id]).unwrap();
                tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES(?1,'message','1','{}')",[&node]).unwrap();
                tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES(?1,?1,'{}',?2,?3,'applied','1','{\"reply\":{\"state\":\"pending\"},\"to_human\":true}')",params![format!("r{n}"),id,node]).unwrap();
            }
            tx.commit().unwrap();
            db
        }
        let mut steps = Vec::new();
        for late in [0, 100, 2_000] {
            let db = setup(late);
            let args = |cursor| ThreadListArgs {
                limit: 1,
                cursor,
                query: None,
                awaiting: Some("human".into()),
                archived: ArchivedFilter::Include,
                tags: vec![],
                cwd: None,
            };
            let mut stmt = db.prepare(awaiting_human_candidates_sql()).unwrap();
            let mut rows = stmt.query(params!["", 2, 17]).unwrap();
            while rows.next().unwrap().is_some() {}
            drop(rows);
            let page_steps = stmt.get_status(rusqlite::StatementStatus::VmStep);
            let first = thread_list_awaiting_human(&db, &args(None)).unwrap();
            assert_eq!(first.items[0].thread_id, "a");
            steps.push(page_steps);
            let cursor = first.next_cursor.unwrap();
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES('rz','oz','{}','z','nz','applied','0','{\"reply\":{\"state\":\"pending\"},\"to_human\":true}')",[]).unwrap();
            let second = thread_list_awaiting_human(&db, &args(Some(cursor))).unwrap();
            assert_eq!(second.items[0].thread_id, "b");
            let mut next = thread_list_awaiting_human(&db, &args(second.next_cursor)).unwrap();
            if late > 0 {
                assert!(next.items[0].thread_id.starts_with("m"));
                for _ in 1..late {
                    next = thread_list_awaiting_human(&db, &args(next.next_cursor)).unwrap();
                }
                assert!(next.items[0].thread_id.starts_with("m"));
                next = thread_list_awaiting_human(&db, &args(next.next_cursor)).unwrap();
            }
            assert_eq!(next.items[0].thread_id, "z");
            assert!(
                next.next_cursor.is_none(),
                "cursor terminates after the final key"
            );
        }
        assert!(
            steps.iter().all(|steps| *steps <= 300),
            "page candidate VM steps exceed fixed bound: {steps:?}"
        );
        assert!(
            steps[2] <= steps[1] + 2,
            "VM steps scale with late-arrival population: {steps:?}"
        );
    }

    #[test]
    fn awaiting_human_legacy_excess_is_bounded_and_readable() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('thread','','','p','m','now')", []).unwrap();
        for index in 0..1000 {
            let node = format!("node-{index}");
            db.execute(
                "INSERT INTO node(id,kind,created_at,payload_json) VALUES(?1,'message',?1,'{}')",
                [&node],
            )
            .unwrap();
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES(?1,?1,'{}','thread',?2,'applied',?1,'{\"reply\":{\"state\":\"pending\"},\"to_human\":true}')", params![format!("receipt-{index}"), node]).unwrap();
        }
        let page = thread_list_awaiting_human(
            &db,
            &ThreadListArgs {
                limit: 1,
                cursor: None,
                query: None,
                awaiting: Some("human".into()),
                archived: ArchivedFilter::Include,
                tags: Vec::new(),
                cwd: None,
            },
        )
        .unwrap();
        assert_eq!(page.items[0].awaiting_human_count, 17);
        assert!(
            !page.items[0].awaiting_human_count_exact,
            "17 is a lower bound, not an exact count"
        );
        assert!(
            page.items[0].awaiting_human_oldest_at.is_none(),
            "oldest is unknown when the bounded sample is inexact"
        );
        let sql = awaiting_human_candidates_sql();
        let mut plan = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let details: Vec<String> = plan
            .query_map(
                params![
                    "",
                    1,
                    (crate::protocol::MAX_HUMAN_REPLY_SUMMARY_DUTIES + 1) as i64
                ],
                |r| r.get(3),
            )
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            details
                .iter()
                .any(|x| x.contains("receipt_human_reply_pending") && x.contains("SEARCH")),
            "{details:?}"
        );
    }

    #[test]
    fn awaiting_human_cursor_observes_receipt_state_churn_between_pages() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        for id in ["a", "b", "c"] {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,'','','p','m','now')", [id]).unwrap();
            let node = format!("n{id}");
            db.execute(
                "INSERT INTO node(id,kind,created_at,payload_json) VALUES(?1,'message','now','{}')",
                [&node],
            )
            .unwrap();
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES(?1,?1,'{}',?2,?3,'applied','now','{\"reply\":{\"state\":\"pending\"},\"to_human\":true}')", params![format!("r{id}"), id, node]).unwrap();
        }
        let args = |cursor| ThreadListArgs {
            limit: 1,
            cursor,
            query: None,
            awaiting: Some("human".into()),
            archived: ArchivedFilter::Include,
            tags: vec![],
            cwd: None,
        };
        let first = thread_list_awaiting_human(&db, &args(None)).unwrap();
        assert_eq!(first.items[0].thread_id, "a");
        db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','replied') WHERE id='rb'", []).unwrap();
        let next = thread_list_awaiting_human(&db, &args(first.next_cursor)).unwrap();
        assert_eq!(next.items[0].thread_id, "c");
        assert!(next.next_cursor.is_none());
    }

    #[test]
    fn awaiting_human_oldest_is_minimum_created_at_and_unknown_when_saturated() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','','','p','m','now')", []).unwrap();
        for index in 0..17 {
            let node = format!("node-{index}");
            db.execute(
                "INSERT INTO node(id,kind,created_at,payload_json) VALUES(?1,'message','now','{}')",
                [&node],
            )
            .unwrap();
            let created = match index {
                0 => "2030-01-01",
                1 => "1999-01-01",
                _ => "2020-01-01",
            };
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES(?1,?1,'{}','t',?2,'applied',?3,'{\"reply\":{\"state\":\"pending\"},\"to_human\":true}')", params![format!("receipt-{index}"), node, created]).unwrap();
        }
        let page = |db: &Connection| {
            thread_list_awaiting_human(
                db,
                &ThreadListArgs {
                    limit: 1,
                    cursor: None,
                    query: None,
                    awaiting: Some("human".into()),
                    archived: ArchivedFilter::Include,
                    tags: vec![],
                    cwd: None,
                },
            )
            .unwrap()
        };
        let saturated = page(&db);
        assert_eq!(saturated.items[0].awaiting_human_count, 17);
        assert!(!saturated.items[0].awaiting_human_count_exact);
        assert_eq!(saturated.items[0].awaiting_human_oldest_at, None);
        db.execute("UPDATE receipt SET result_json='{\"reply\":{\"state\":\"replied\"},\"to_human\":true}' WHERE id='receipt-16'", []).unwrap();
        let exact = page(&db);
        assert_eq!(exact.items[0].awaiting_human_count, 16);
        assert!(exact.items[0].awaiting_human_count_exact);
        assert_eq!(
            exact.items[0].awaiting_human_oldest_at.as_deref(),
            Some("1999-01-01")
        );
    }

    #[test]
    fn awaiting_human_live_cursor_handles_add_reply_cancel_archive_and_scope() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('0','','','p','m','now'),('a','','','p','m','now'),('b','','','p','m','now'),('c','','','p','m','now'),('d','','','p','m','now'); INSERT INTO node(id,kind,created_at,payload_json) VALUES('n0','message','0','{}'),('na','message','1','{}'),('nb','message','2','{}'),('nc','message','3','{}'),('nd','message','4','{}');").unwrap();
        let insert = "INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES(?1,?1,'{}',?2,?3,'applied',?4,'{\"reply\":{\"state\":\"pending\"},\"to_human\":true}')";
        for (id, thread, node) in [("ra", "a", "na"), ("rb", "b", "nb"), ("rc", "c", "nc")] {
            db.execute(insert, params![id, thread, node, id]).unwrap();
        }
        let args = |cursor, archived| ThreadListArgs {
            limit: 1,
            cursor,
            query: None,
            awaiting: Some("human".into()),
            archived,
            tags: vec![],
            cwd: None,
        };
        let first = thread_list_awaiting_human(&db, &args(None, ArchivedFilter::Exclude)).unwrap();
        assert_eq!(first.items[0].thread_id, "a");
        let cursor = first.next_cursor.unwrap();
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES('r0','o0','{}','0','n0','applied','0','{\"reply\":{\"state\":\"pending\"},\"to_human\":true}')",[]).unwrap();
        db.execute("UPDATE thread SET archived_at='now' WHERE id='b'", [])
            .unwrap();
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='c'", [])
            .unwrap();
        let next =
            thread_list_awaiting_human(&db, &args(Some(cursor.clone()), ArchivedFilter::Exclude))
                .unwrap();
        assert_eq!(
            next.items.len(),
            0,
            "bounded archived/deleted candidates may yield an empty page"
        );
        assert!(
            next.items.iter().all(|item| item.thread_id != "0"),
            "a late key before the live cursor is deferred to a fresh listing"
        );
        assert!(
            next.next_cursor.is_some(),
            "empty page still advances over examined candidate keys"
        );
        db.execute(insert, params!["rd", "d", "nd", "0"]).unwrap();
        let hidden =
            thread_list_awaiting_human(&db, &args(next.next_cursor, ArchivedFilter::Exclude))
                .unwrap();
        assert!(hidden.items.is_empty());
        assert!(hidden.next_cursor.is_some());
        let last =
            thread_list_awaiting_human(&db, &args(hidden.next_cursor, ArchivedFilter::Exclude))
                .unwrap();
        assert_eq!(last.items[0].thread_id, "d");
        let before = thread_list_awaiting_human(&db, &args(None, ArchivedFilter::Exclude)).unwrap();
        assert_eq!(
            before.items[0].thread_id, "0",
            "a fresh listing includes late keys before the old cursor"
        );
        let wrong_scope =
            thread_list_awaiting_human(&db, &args(Some(cursor.clone()), ArchivedFilter::Include));
        assert!(wrong_scope.is_err());
        let mut old = PageCursor {
            version: 1,
            scope: "thread.list.awaiting_human".into(),
            filter: "human:include".into(),
            last_sort_key: "a".into(),
            watermark: 1,
            revision: 0,
            encoding: None,
        };
        let token = old.encode().unwrap();
        assert!(
            PageCursor::decode_live(
                &token,
                "thread.list.awaiting_human.live.v2",
                "human:include",
                0,
                None
            )
            .is_err()
        );
        old.version = 2;
        let token = old.encode().unwrap();
        assert!(
            PageCursor::decode_live(
                &token,
                "thread.list.awaiting_human.live.v2",
                "human:include",
                0,
                None
            )
            .is_err(),
            "wrong scope is rejected"
        );
        assert!(
            PageCursor::decode_live(
                "not-a-cursor",
                "thread.list.awaiting_human.live.v2",
                "human:include",
                0,
                None
            )
            .is_err(),
            "malformed cursor is rejected, never silently restarted"
        );
    }

    #[test]
    fn actual_correlated_thread_list_seeks_latest_run_once_without_per_run_sort() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        let plans: Vec<String> = db
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                thread_summary::page_sql(ArchivedFilter::Exclude, 0, false)
            ))
            .unwrap()
            .query_map(params![100_i64, "", "", 50], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let seeks: Vec<_> = plans
            .iter()
            .filter(|p| p.contains("run_thread_created"))
            .collect();
        assert_eq!(seeks.len(), 1, "{plans:?}");
        assert!(
            plans
                .iter()
                .any(|p| p.contains("SEARCH latest") && p.contains("INDEX")),
            "{plans:?}"
        );
        assert!(!plans.iter().any(|p| p.contains("SCAN r")), "{plans:?}");
        assert!(
            plans.iter().any(|p| p.contains("shell_active_owner")),
            "{plans:?}"
        );
        // The outer Thread pagination may sort; the scalar Run lookup must use
        // its covering index instead of reading every Run in each Thread.
    }

    fn outer_plan(db: &Connection, mode: ArchivedFilter) -> Vec<String> {
        db.prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            thread_summary::page_sql(mode, 0, false)
        ))
        .unwrap()
        .query_map(params![100_i64, "", "", 50], |r| {
            Ok(format!(
                "{} [parent={}]",
                r.get::<_, String>(3)?,
                r.get::<_, i64>(1)?
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    #[test]
    fn each_archived_mode_walks_its_partial_index_in_order_without_a_sort() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        for (mode, index) in [
            (ArchivedFilter::Exclude, "threads_visible_updated"),
            (ArchivedFilter::Only, "threads_archived_updated"),
        ] {
            let plans = outer_plan(&db, mode);
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("SCAN t") && p.contains(index)),
                "{mode:?} must walk {index} in order: {plans:?}"
            );
            assert!(
                !plans
                    .iter()
                    .any(|p| p.contains("TEMP B-TREE") && p.ends_with("[parent=0]")),
                "{mode:?} must not sort the Threads: {plans:?}"
            );
        }
        let include = outer_plan(&db, ArchivedFilter::Include);
        assert!(
            !include
                .iter()
                .any(|p| p.contains("threads_archived_updated")),
            "{include:?}"
        );
    }

    #[test]
    fn default_page_cost_does_not_grow_with_archived_threads() {
        let steps = |archived: u32| {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch(&format!(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<{})
                 INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at,archived_at)
                 SELECT printf('a%05d',x),'/','','p','m','2026-11-02T00:00:00Z','2026-11-03T00:00:00Z' FROM n;
                 WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<60)
                 INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)
                 SELECT printf('v%05d',x),'/','','p','m','2026-11-01T00:00:00Z' FROM n;",
                archived
            ))
            .unwrap();
            // Archived rows are the newest by `updated_at`: a residual filter would
            // have to walk past all of them before reaching one visible Thread.
            let mut statement = db
                .prepare(&thread_summary::page_sql(ArchivedFilter::Exclude, 0, false))
                .unwrap();
            let rows = statement
                .query_map(params![1_000_000_i64, "", "", 50], |_| Ok(()))
                .unwrap()
                .count();
            assert_eq!(rows, 50);
            statement.get_status(rusqlite::StatementStatus::VmStep)
        };
        let (few, many) = (steps(20), steps(3000));
        assert!(
            many < few + few / 4,
            "default page read grew with archived Threads: {few} -> {many} VM steps"
        );
    }

    fn plan_of(db: &Connection, mode: ArchivedFilter, tags: usize, cwd: bool) -> Vec<String> {
        let mut binds: Vec<rusqlite::types::Value> = vec![
            100_i64.into(),
            String::new().into(),
            String::new().into(),
            50_i64.into(),
        ];
        binds.extend((0..tags).map(|n| format!("tag{n}").into()));
        if cwd {
            binds.push("/work".to_owned().into());
        }
        db.prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            thread_summary::page_sql(mode, tags, cwd)
        ))
        .unwrap()
        .query_map(rusqlite::params_from_iter(binds), |r| {
            Ok(format!(
                "{} [parent={}]",
                r.get::<_, String>(3)?,
                r.get::<_, i64>(1)?
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    /// One filtered page in a database holding `unrelated` Threads of another cwd and
    /// 40 tagged Threads (`tag0` on all, `tag1` on the first 30).
    fn filtered_steps(unrelated: u32, tags: usize, cwd: bool) -> i32 {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch(&format!(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<{unrelated})
             INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)
             SELECT printf('u%05d',x),'/other','','p','m',printf('2026-11-02T00:%02d:00Z',x%60) FROM n;
             WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<40)
             INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)
             SELECT printf('m%05d',x),'/work','','p','m',printf('2026-11-01T00:%02d:00Z',x) FROM n;
             INSERT INTO thread_tag(thread_id,tag) SELECT id,'tag0' FROM thread WHERE id LIKE 'm%';
             INSERT INTO thread_tag(thread_id,tag) SELECT id,'tag1' FROM thread WHERE id<='m00030' AND id LIKE 'm%';"
        ))
        .unwrap();
        let mut binds: Vec<rusqlite::types::Value> = vec![
            1_000_000_i64.into(),
            String::new().into(),
            String::new().into(),
            21_i64.into(),
        ];
        binds.extend((0..tags).map(|n| format!("tag{n}").into()));
        if cwd {
            binds.push("/work".to_owned().into());
        }
        let mut statement = db
            .prepare(&thread_summary::page_sql(
                ArchivedFilter::Exclude,
                tags,
                cwd,
            ))
            .unwrap();
        let rows = statement
            .query_map(rusqlite::params_from_iter(binds), |_| Ok(()))
            .unwrap()
            .count();
        assert_eq!(rows, 21);
        statement.get_status(rusqlite::StatementStatus::VmStep)
    }

    #[test]
    fn tag_and_cwd_filters_read_their_own_index_never_the_whole_thread_table() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        for mode in [
            ArchivedFilter::Exclude,
            ArchivedFilter::Only,
            ArchivedFilter::Include,
        ] {
            let plans = plan_of(&db, mode, 2, false);
            if mode == ArchivedFilter::Exclude {
                eprintln!(
                    "EXPLAIN tags=2 exclude: {:#?}",
                    plans
                        .iter()
                        .filter(|p| p.contains("g0")
                            || p.contains("g1")
                            || p.contains("SEARCH t ")
                            || p.contains("TEMP B-TREE"))
                        .collect::<Vec<_>>()
                );
                eprintln!(
                    "EXPLAIN cwd exclude: {:#?}",
                    plan_of(&db, mode, 0, true)
                        .iter()
                        .filter(|p| p.contains("SEARCH t ")
                            || p.contains("SCAN t")
                            || p.contains("TEMP B-TREE"))
                        .collect::<Vec<_>>()
                );
            }
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("SEARCH g0") && p.contains("thread_tag_by_tag (tag=?)")),
                "{mode:?} must start from the first tag's members: {plans:?}"
            );
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("SEARCH t USING INDEX sqlite_autoindex_thread_1 (id=?)")),
                "{mode:?} must reach each member by primary key: {plans:?}"
            );
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("SEARCH g1") && p.contains("(thread_id=? AND tag=?)")),
                "{mode:?} must probe each further tag by primary key: {plans:?}"
            );
            assert!(
                !plans
                    .iter()
                    .any(|p| p.starts_with("SCAN t") || p.starts_with("SCAN g")),
                "{mode:?} must not scan a Thread or tag table: {plans:?}"
            );
            let cwd = plan_of(&db, mode, 0, true);
            assert!(
                cwd.iter()
                    .any(|p| p.contains("SEARCH t") && p.contains("threads_cwd_updated (cwd=?)")),
                "{mode:?} must seek the cwd index: {cwd:?}"
            );
            assert!(
                !cwd.iter().any(|p| {
                    (p.contains("TEMP B-TREE") && p.ends_with("[parent=0]"))
                        || p.starts_with("SCAN t")
                }),
                "{mode:?} cwd page is already in list order: {cwd:?}"
            );
        }
        let both = plan_of(&db, ArchivedFilter::Exclude, 1, true);
        assert!(
            both.iter().any(|p| p.contains("thread_tag_by_tag")),
            "{both:?}"
        );
    }

    /// One page of 21 rows from a tag with `members` Threads, each with a Run and a Turn
    /// (the summary columns cost most per row).
    fn member_steps(members: u32) -> i32 {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch(&format!(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<{members})
             INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)
             SELECT printf('m%06d',x),'/work','','p','m',printf('2026-11-01T%02d:%02d:00Z',(x/60)%24,x%60) FROM n;
             INSERT INTO thread_tag(thread_id,tag) SELECT id,'big' FROM thread;
             INSERT INTO run(id,thread_id,state,created_at) SELECT 'r'||id,id,'completed',updated_at FROM thread;
             INSERT INTO turn(id,thread_id,run_id,state,created_at) SELECT 'u'||id,id,'r'||id,'completed',updated_at FROM thread;"
        ))
        .unwrap();
        let mut statement = db
            .prepare(&thread_summary::page_sql(ArchivedFilter::Exclude, 1, false))
            .unwrap();
        let rows = statement
            .query_map(params![1_000_000_i64, "", "", 21_i64, "big"], |_| Ok(()))
            .unwrap()
            .count();
        assert_eq!(rows, 21);
        statement.get_status(rusqlite::StatementStatus::VmStep)
    }

    #[test]
    fn tag_page_reads_summary_columns_for_the_page_not_for_every_member() {
        let (few, many) = (member_steps(100), member_steps(5000));
        eprintln!("tag page, 21 rows: 100 members {few} VM steps, 5000 members {many} VM steps");
        // The ID sort costs a few dozen steps per member; the summary subqueries
        // (thousands of steps per row) run for the 21 returned rows only.
        let per_member = (many - few) / 4900;
        assert!(per_member < 150, "{per_member} VM steps per added member");
    }

    #[test]
    fn cwd_names_are_one_index_seek_each_whatever_a_directory_holds() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        let plan: Vec<String> = db
            .prepare(&format!("EXPLAIN QUERY PLAN {}", tags::NEXT_CWD_SQL))
            .unwrap()
            .query_map(["/a"], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        eprintln!("EXPLAIN cwds: {plan:?}");
        assert!(
            plan.iter()
                .any(|p| p.contains("threads_cwd_updated (cwd>?)")),
            "{plan:?}"
        );
        assert!(!plan.iter().any(|p| p.contains("TEMP B-TREE")), "{plan:?}");
        // 40 directories; one holds few Threads and many are archived, the other fixture many.
        let measure = |per_dir: u32, archived: bool| {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch(&format!(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<{})
                 INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at,archived_at)
                 SELECT printf('t%06d',x),printf('/dir%02d',x%50),'','p','m','2026-11-01T00:00:00Z',
                        {} FROM n;",
                per_dir * 50,
                if archived {
                    "'2026-11-02T00:00:00Z'"
                } else {
                    "NULL"
                }
            ))
            .unwrap();
            let mut next = db.prepare(tags::NEXT_CWD_SQL).unwrap();
            let mut after = String::new();
            for _ in 0..40 {
                after = next
                    .query_row([&after], |row| row.get::<_, String>(0))
                    .unwrap();
            }
            next.get_status(rusqlite::StatementStatus::VmStep)
        };
        let (small, large, all_archived) =
            (measure(2, false), measure(200, false), measure(200, true));
        eprintln!(
            "40 cwd seeks: 2 Threads per directory {small} VM steps, 200 per directory {large}, 200 per directory all archived {all_archived}"
        );
        assert_eq!(small, large);
        assert_eq!(large, all_archived);
    }

    #[test]
    fn filtered_page_cost_does_not_grow_with_unrelated_threads() {
        for (tags, cwd) in [(1, false), (2, false), (0, true), (2, true)] {
            let (few, many) = (
                filtered_steps(50, tags, cwd),
                filtered_steps(6000, tags, cwd),
            );
            eprintln!(
                "filtered page tags={tags} cwd={cwd}: 50 unrelated Threads {few} VM steps, 6000 unrelated Threads {many} VM steps"
            );
            assert!(
                many < few + few / 10,
                "tags={tags} cwd={cwd}: filtered page grew with unrelated Threads: {few} -> {many} VM steps"
            );
        }
    }

    #[test]
    fn page_tags_and_name_pages_read_by_key_not_by_table_size() {
        let measure = |unrelated: u32, members: u32| {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch(&format!(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<{unrelated})
                 INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)
                 SELECT printf('t%05d',x),'/','','p','m','2026-11-01T00:00:00Z' FROM n;
                 INSERT INTO thread_tag(thread_id,tag) SELECT id,'x'||(rowid%64) FROM thread WHERE rowid<=50;
                 WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<{members})
                 INSERT INTO thread_tag(thread_id,tag) SELECT printf('t%05d',x),'name'||printf('%02d',x%50) FROM n WHERE x>50;"
            ))
            .unwrap();
            let ids: Vec<String> = (1..=50).map(|n| format!("t{n:05}")).collect();
            let encoded = serde_json::to_string(&ids).unwrap();
            let mut page = db.prepare(tags::FOR_THREADS_SQL).unwrap();
            let rows = page.query_map([encoded], |_| Ok(())).unwrap().count();
            assert_eq!(rows, 50);
            let mut next = db.prepare(tags::NEXT_NAME_SQL).unwrap();
            let mut after = String::new();
            for _ in 0..40 {
                after = next
                    .query_row([&after], |row| row.get::<_, String>(0))
                    .unwrap();
            }
            (
                page.get_status(rusqlite::StatementStatus::VmStep),
                next.get_status(rusqlite::StatementStatus::VmStep),
            )
        };
        let (small, large) = (measure(200, 200), measure(8000, 8000));
        eprintln!(
            "page tags (50 Threads) / 40 name seeks: 200 Threads {small:?} VM steps, 8000 Threads {large:?} VM steps"
        );
        assert!(
            large.0 < small.0 + small.0 / 10,
            "page tags grew with stored Threads: {small:?} -> {large:?}"
        );
        assert!(
            large.1 < small.1 + small.1 / 10,
            "name page grew with tag membership: {small:?} -> {large:?}"
        );
    }
}

#[cfg(test)]
mod activity_list_tests {
    use super::*;
    use crate::activity::ActivityPhase;
    #[test]
    fn many_threads_keep_activity_paged_and_indexed() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2000)
          INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)
          SELECT printf('t%04d',x),'Synthetic','/','','p','m','2026-10-29T00:00:00Z' FROM n;
          INSERT INTO run(id,thread_id,state,created_at) SELECT 'r'||id,id,'live',updated_at FROM thread;
          INSERT INTO turn(id,thread_id,run_id,state,created_at) SELECT 'u'||id,id,'r'||id,'completed',updated_at FROM thread;
          INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)
          SELECT 's'||id,'r'||id,'u'||id,'op'||id,'running','/','{}','UTF-8','UTF-8','closed',updated_at FROM thread;").unwrap();
        let begin = std::time::Instant::now();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = thread_list(
                &db,
                &ThreadListArgs {
                    limit: 50,
                    cursor,
                    query: None,
                    awaiting: None,
                    archived: Default::default(),
                    tags: Vec::new(),
                    cwd: None,
                },
            )
            .unwrap();
            for item in page.items {
                assert_eq!(item.state, "completed");
                assert_eq!(item.activity, ActivityPhase::Background);
                assert!(seen.insert(item.thread_id));
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen.len(), 2000);
        eprintln!(
            "issue44: 2000 background Threads, 40 pages: {:?}",
            begin.elapsed()
        );
    }

    #[test]
    fn list_and_footer_share_owned_background_and_finalization_projection() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('parent','Parent','/','','p','m','2026-10-29T00:00:00Z'),('child','Child','/','','p','m','2026-10-29T00:00:00Z');
          INSERT INTO run(id,thread_id,state,created_at) VALUES('r','parent','live','2026-10-29T00:00:00Z');
          INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','parent','r','completed','2026-10-29T00:00:00Z');
          INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)
          VALUES('s','r','u','op','running','/','{}','UTF-8','UTF-8','closed','2026-10-29T00:00:00Z');").unwrap();
        let verify = |expected, native| {
            let page = thread_list(&db, &ThreadListArgs::default()).unwrap();
            let item = page.items.iter().find(|t| t.thread_id == "parent").unwrap();
            assert_eq!(item.state, native);
            assert_eq!(item.activity, expected);
            assert_eq!(
                crate::frontend::activity::project(&db, "parent")
                    .unwrap()
                    .snapshot
                    .phase,
                expected
            );
        };
        verify(ActivityPhase::Background, "completed");
        db.execute("UPDATE shell_execution SET state='exited'", [])
            .unwrap();
        verify(ActivityPhase::Continuing, "completed");
        db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('kid','child','live','2026-10-29T00:00:00Z')", []).unwrap();
        verify(ActivityPhase::Continuing, "completed");
        db.execute("UPDATE run SET state='stopping' WHERE id='kid'", [])
            .unwrap();
        verify(ActivityPhase::Continuing, "completed");
        db.execute("UPDATE run SET state='completed' WHERE id='kid'", [])
            .unwrap();
        verify(ActivityPhase::Continuing, "completed");
        db.execute("UPDATE run SET state='completed' WHERE id='r'", [])
            .unwrap();
        verify(ActivityPhase::Completed, "completed");
        db.execute("UPDATE run SET state='live' WHERE id='r'", [])
            .unwrap();
        db.execute("UPDATE turn SET state='running' WHERE id='u'", [])
            .unwrap();
        db.execute_batch("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at,error_json)
          VALUES('a','u',1,0,'p','m',0,'failed','{}','2026-10-29T00:00:00Z','{\"automatic_retry\":{\"number\":1,\"limit\":2,\"delay_ms\":10,\"partial_response_discarded\":false}}');").unwrap();
        verify(ActivityPhase::Running, "running");
        db.execute("UPDATE model_attempt SET generation=0", [])
            .unwrap();
        verify(ActivityPhase::Retrying, "running");
        db.execute("UPDATE model_attempt SET state='inflight'", [])
            .unwrap();
        verify(ActivityPhase::AwaitingModel, "running");
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)
          SELECT 'bulk-'||value,'Synthetic','/','','p','m','2026-10-28T00:00:00Z' FROM json_each('[0,1,2]');").unwrap();
        let idle = thread_list(&db, &ThreadListArgs::default()).unwrap();
        assert!(
            idle.items
                .iter()
                .filter(|t| t.thread_id.starts_with("bulk-"))
                .all(|t| t.activity == ActivityPhase::Idle)
        );
    }
}

#[cfg(test)]
mod receipt_wait_tests {
    use super::*;
    use crate::control::receipt::MAX_OUTCOME_BYTES;

    async fn fixture() -> (tempfile::TempDir, DbWorker, Store) {
        let root = tempfile::tempdir().unwrap();
        let db = DbWorker::open(root.path().join("receipt.db"))
            .await
            .unwrap();
        let config = Config::parse("default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
        let store = Store::new(
            db.clone(),
            Arc::new(config),
            root.path().into(),
            "current".into(),
        );
        (root, db, store)
    }

    async fn seed_pending(db: &DbWorker, old_rows: u32, last_reserved: u64) {
        db.call(move |db| {
            let tx = db.transaction()?;
            for i in 0..old_rows {
                tx.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes) VALUES(?1,'old','null','hash','pending','2000-01-01',0)", [format!("old:pending:{i}")])?;
            }
            for i in 0..30 {
                tx.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes) VALUES(?1,'current','null','hash','pending','2000-01-01',?2)", params![format!("current:pending:{i}"), MAX_OUTCOME_BYTES as i64])?;
            }
            tx.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes) VALUES('current:last','current','null','hash','pending','2000-01-01',?1)", [last_reserved as i64])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
    }

    #[test]
    fn request_fingerprint_is_canonical_and_reused() {
        let request = Request::new(
            "current:hash",
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            }),
        );
        let expected =
            hash(serde_json::to_vec(&json!({"command":request.command,"caller":null})).unwrap());
        let cache = std::sync::OnceLock::new();
        let first = request_fingerprint(&cache, &request, None).unwrap();
        let second = request_fingerprint(&cache, &request, None).unwrap();
        assert_eq!(first, expected);
        assert_eq!(
            first.as_ptr(),
            second.as_ptr(),
            "retries reuse the stored digest"
        );
    }

    #[tokio::test]
    async fn waits_outside_writer_until_a_later_window_reclaims_space() {
        let (_root, db, store) = fixture().await;
        seed_pending(&db, 1024, 6 * 1024 * 1024).await;
        db.call(|db| {
            let body = json!({"result":"x".repeat(4 * 1024 * 1024)}).to_string();
            db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('old:done','old','null','hash','completed','2000-01-01',?1,0)", [body])?;
            Ok(())
        }).await.unwrap();
        db.take_background_steps();
        let background = store.background.lock().await;
        let waiting_store = store.clone();
        let task = tokio::spawn(async move {
            waiting_store
                .reserve_internal("current:new".into(), json!({"test":1}))
                .await
        });
        tokio::task::yield_now().await;
        let present = db
            .call(|db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id='current:new'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        tokio::task::yield_now().await;
        assert_eq!(present, 0);
        assert!(
            !task.is_finished(),
            "the request must wait instead of returning storage_limit"
        );
        db.call(|db| {
            db.execute(
                "INSERT INTO app_meta(key,value) VALUES('writer-responsive','yes')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        drop(background);
        let result = crate::test_watchdog::watchdog("receipt admission completes", task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(result, RuntimeReservation::New));
        assert!(matches!(
            store
                .reserve_internal("current:new".into(), json!({"test":1}))
                .await
                .unwrap(),
            RuntimeReservation::Pending
        ));
        let rows = db
            .call(|db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id='old:done'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(rows, 0);
        assert!(
            db.take_background_steps().len() >= 2,
            "the later row requires multiple background windows"
        );
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn space_freed_while_waiting_is_rechecked_before_more_reclamation() {
        let (_root, db, store) = fixture().await;
        seed_pending(&db, 0, 5 * 1024 * 1024).await;
        db.call(|db| {
            let body = json!({"result":"x".repeat(2 * 1024 * 1024)}).to_string();
            for i in 0..2 {
                db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES(?1,'old','null','hash','completed','2000-01-01',?2,0)", params![format!("old:done:{i}"), &body])?;
            }
            Ok(())
        }).await.unwrap();
        db.take_background_steps();
        let background = store.background.lock().await;
        let waiting_store = store.clone();
        let pressure_seen = Arc::new(tokio::sync::Notify::new());
        let notify = pressure_seen.clone();
        let task = tokio::spawn(async move {
            waiting_store
                .receipt_job(None, None, move |db, pressure| {
                    let tx = db.transaction()?;
                    match super::super::receipt::reserve(
                        &tx,
                        "current",
                        "current:new",
                        "null",
                        "hash",
                        "now",
                        Admission::Normal,
                    ) {
                        Ok(()) => {
                            tx.commit()?;
                            Ok(Ok(()))
                        }
                        Err(error) if error.code == ErrorCode::StorageLimit => {
                            tx.commit()?;
                            *pressure = true;
                            notify.notify_one();
                            Ok(Err(error))
                        }
                        Err(error) => Ok(Err(error)),
                    }
                })
                .await
        });
        crate::test_watchdog::watchdog("first capacity check", pressure_seen.notified())
            .await
            .unwrap();
        db.call(|db| {
            db.execute(
                "DELETE FROM operation_receipt WHERE operation_id='old:done:0'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        drop(background);
        crate::test_watchdog::watchdog("admission after another cleanup", task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let remaining = db
            .call(|db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id='old:done:1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(remaining, 1, "no additional receipt needs to expire");
        assert!(db.take_background_steps().is_empty());
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn execute_waits_for_a_later_window_and_applies_only_once() {
        let (_root, db, store) = fixture().await;
        seed_pending(&db, 1024, 6 * 1024 * 1024).await;
        db.call(|db| {
            let body = json!({"result":"x".repeat(4 * 1024 * 1024)}).to_string();
            db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('old:done','old','null','hash','completed','2000-01-01',?1,0)", [body])?;
            Ok(())
        }).await.unwrap();
        let request = Request::new(
            "current:execute",
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            }),
        );
        let background = store.background.lock().await;
        let waiting_store = store.clone();
        let waiting_request = request.clone();
        let task = tokio::spawn(async move { waiting_store.execute(waiting_request, None).await });
        tokio::task::yield_now().await;
        let threads = db
            .call(|db| {
                Ok(db.query_row("SELECT count(*) FROM thread", [], |row| {
                    row.get::<_, i64>(0)
                })?)
            })
            .await
            .unwrap();
        tokio::task::yield_now().await;
        assert_eq!(threads, 0);
        assert!(
            !task.is_finished(),
            "ordinary admission must wait, not reject"
        );
        drop(background);
        let result = crate::test_watchdog::watchdog("receipt admission completes", task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let replay = store.execute(request, None).await.unwrap();
        assert_eq!(result.result, replay.result);
        let threads = db
            .call(|db| {
                Ok(db.query_row("SELECT count(*) FROM thread", [], |row| {
                    row.get::<_, i64>(0)
                })?)
            })
            .await
            .unwrap();
        assert_eq!(threads, 1, "same-ID replay must not repeat the mutation");
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn ordinary_maintenance_releases_the_requester_between_windows() {
        let (_root, db, store) = fixture().await;
        seed_pending(&db, 2048, MAX_OUTCOME_BYTES).await;
        let initial = store.background.lock().await;
        let (collector_started_tx, collector_started_rx) = tokio::sync::oneshot::channel();
        let collecting_store = store.clone();
        let collector = tokio::spawn(async move {
            collector_started_tx.send(()).unwrap();
            collecting_store.collect_receipts().await
        });
        crate::test_watchdog::watchdog("collector queued", collector_started_rx)
            .await
            .unwrap()
            .unwrap();
        let (observer_started_tx, observer_started_rx) = tokio::sync::oneshot::channel();
        let (acquired_tx, acquired_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let observer_store = store.clone();
        let observer = tokio::spawn(async move {
            observer_started_tx.send(()).unwrap();
            let _guard = observer_store.background.lock().await;
            acquired_tx.send(()).unwrap();
            let _ = release_rx.await;
        });
        crate::test_watchdog::watchdog("observer queued", observer_started_rx)
            .await
            .unwrap()
            .unwrap();
        drop(initial);
        crate::test_watchdog::watchdog("requester released", acquired_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !collector.is_finished(),
            "maintenance has more windows while another requester holds the lane"
        );
        release_tx.send(()).unwrap();
        let pass = crate::test_watchdog::watchdog("maintenance completed", collector)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!pass.deferred);
        crate::test_watchdog::watchdog("observer completed", observer)
            .await
            .unwrap()
            .unwrap();
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn exhausted_background_pass_returns_the_original_capacity_error() {
        let (_root, db, store) = fixture().await;
        seed_pending(&db, 0, MAX_OUTCOME_BYTES).await;
        db.take_background_steps();
        let error = crate::test_watchdog::watchdog(
            "receipt admission completes",
            store.reserve_internal("current:new".into(), json!({"test":2})),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::StorageLimit);
        assert!(
            !db.take_background_steps().is_empty(),
            "refusal follows a background pass"
        );
        let rows = db
            .call(|db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id='current:new'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(rows, 0);
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn validation_byte_limit_resumes_before_the_unexamined_candidate() {
        let (_root, db, store) = fixture().await;
        seed_pending(&db, 0, 5 * 1024 * 1024).await;
        db.call(|db| {
            let body = json!({"result":"x".repeat(6 * 1024 * 1024)}).to_string();
            for i in 0..2 {
                db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES(?1,'current','null','hash','completed','2000-01-01',?2,0)", params![format!("current:done:{i}"), &body])?;
            }
            Ok(())
        }).await.unwrap();
        db.take_background_steps();
        let result = crate::test_watchdog::watchdog(
            "receipt admission completes",
            store.reserve_internal("current:new".into(), json!({"test":3})),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, RuntimeReservation::New));
        let expired = db.call(|db| Ok(db.query_row("SELECT count(*) FROM operation_receipt WHERE operation_id LIKE 'current:done:%' AND result_ref_json='0'", [], |row| row.get::<_, i64>(0))?)).await.unwrap();
        assert_eq!(
            expired, 2,
            "both individually valid bodies must be reclaimed"
        );
        assert!(db.take_background_steps().len() >= 2);
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_change_behind_cursor_restarts_an_exhausted_pass() {
        let (_root, db, store) = fixture().await;
        seed_pending(&db, 1024, 6 * 1024 * 1024).await;
        db.call(|db| {
            let body = json!({"result":"x".repeat(4 * 1024 * 1024)}).to_string();
            db.execute("UPDATE operation_receipt SET result_ref_json=?1 WHERE operation_id='old:pending:0'", [body])?;
            Ok(())
        }).await.unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let result = crate::test_watchdog::watchdog("receipt admission completes", store.receipt_job(None, None, move |db, pressure| {
            if observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                db.execute("UPDATE operation_receipt SET state='completed' WHERE operation_id='old:pending:0'", [])?;
            }
            let tx = db.transaction()?;
            let result = super::super::receipt::reserve(&tx, "current", "current:new", "null", "hash", "now", Admission::Normal);
            match result {
                Ok(()) => { tx.commit()?; Ok(Ok(())) }
                Err(error) if error.code == ErrorCode::StorageLimit => { tx.commit()?; *pressure = true; Ok(Err(error)) }
                Err(error) => Ok(Err(error)),
            }
        })).await.unwrap();
        result.unwrap();
        assert!(calls.load(std::sync::atomic::Ordering::SeqCst) >= 4);
        let remaining = db
            .call(|db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id='old:pending:0'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(remaining, 0);
        db.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod agents_reference_tests {
    use super::*;

    async fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let db = DbWorker::open(root.path().join("fixture.db"))
            .await
            .unwrap();
        let config = Config::parse("default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
        let store = Store::new(db, Arc::new(config), root.path().into(), "fixture".into());
        (root, store)
    }
    fn req(command: Command) -> Request {
        Request::new(format!("fixture:{}", uuid::Uuid::new_v4()), command)
    }
    fn create(agents: Option<&str>, cwd: Option<&str>, parent: Option<String>) -> Command {
        Command::ThreadCreate(ThreadCreateArgs {
            agents: agents.map(str::to_owned),
            cwd: cwd.map(str::to_owned),
            parent_id: parent,
            connection: None,
            model: None,
            effort: None,
        })
    }
    async fn details(store: &Store, id: &str) -> Value {
        store
            .execute(
                req(Command::ThreadGet(ThreadTarget {
                    thread_id: Some(id.into()),
                })),
                None,
            )
            .await
            .unwrap()
            .result
    }

    #[tokio::test]
    async fn pending_reply_admission_rejects_seventeenth_and_reopens_after_reply() {
        let (root, store) = fixture().await;
        let thread = store
            .execute(req(create(None, None, None)), None)
            .await
            .unwrap()
            .result["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let send = |text: String| {
            Command::MessageSend(MessageSendArgs {
                human: false,
                resume_recovery: false,
                to_human: false,
                expect_reply: true,
                sender_label: None,
                thread_id: Some(thread.clone()),
                text,
                deliver: Deliver::Next,
            })
        };
        let mut messages = Vec::new();
        for n in 0..crate::protocol::MAX_PENDING_REPLY_DUTIES {
            let result = store
                .execute(req(send(format!("question {n}"))), None)
                .await
                .unwrap()
                .result;
            messages.push(result["message_id"].as_str().unwrap().to_owned());
        }
        let error = store
            .execute(req(send("seventeenth".into())), None)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ReplyLimit);
        assert_eq!(error.details.as_ref().unwrap()["limit"], 16);
        assert_eq!(error.details.as_ref().unwrap()["current_count"], 16);
        assert!(error.message.contains("reply to or cancel"));
        let retained_thread = thread.clone();
        let retained = store.database.call(move |db| Ok(db.query_row(
            "SELECT count(*) FROM receipt WHERE target_thread_id=?1 AND json_extract(result_json,'$.reply.state')='pending' AND COALESCE(json_extract(result_json,'$.to_human'),0)=0",
            [&retained_thread], |row| row.get::<_, i64>(0),
        )?)).await.unwrap();
        assert_eq!(
            retained, 16,
            "refusing the 17th must retain all pre-existing duties"
        );
        let reply_store = store.clone();
        let message = messages[0].clone();
        store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                tx.execute(
                    "UPDATE receipt SET state='applied' WHERE message_node_id=?1",
                    [&message],
                )?;
                reply_store
                    .reply(
                        &tx,
                        &MessageReplyArgs {
                            message_id: message,
                            text: "done".into(),
                            sender_label: None,
                        },
                        None,
                        "fixture:reply",
                        "now",
                    )
                    .map_err(StorageError::from)?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let accepted = store
            .execute(req(send("after reply".into())), None)
            .await
            .unwrap();
        assert!(!accepted.result["message_id"].as_str().unwrap().is_empty());
        drop(store);
        let _ = root;
    }

    #[tokio::test]
    async fn explicit_agents_resolution_inheritance_validation_and_receipt_replay() {
        let (root, store) = fixture().await;
        std::fs::create_dir(root.path().join("project")).unwrap();
        std::fs::write(root.path().join("project/role.md"), "role instructions").unwrap();
        let request = req(create(Some("./role.md"), Some("project"), None));
        let first = store.execute(request.clone(), None).await.unwrap().result;
        let thread = first["thread_id"].as_str().unwrap();
        let expected = root
            .path()
            .join("project")
            .join("role.md")
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(first["agents_path"], expected);
        let inherited = store
            .execute(req(create(None, None, Some(thread.into()))), None)
            .await
            .unwrap()
            .result;
        assert_eq!(inherited["agents_path"], expected);
        // The low-level fork continues sharing history but uses current settings.
        let target = thread.to_owned();
        store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                history::append_node(
                    &tx,
                    &target,
                    "source",
                    None,
                    "user",
                    &json!({"role":"user","content":[{"type":"input_text","text":"fork source"}]}),
                    "now",
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let fork = store
            .execute(
                req(Command::ThreadFork(ThreadForkArgs {
                    agents: None,
                    thread_id: thread.into(),
                    node_id: "source".into(),
                    cwd: None,
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert_eq!(fork["agents_path"], expected);
        let override_fork = store
            .execute(
                req(Command::ThreadFork(ThreadForkArgs {
                    agents: Some("role.md".into()),
                    thread_id: thread.into(),
                    node_id: "source".into(),
                    cwd: None,
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert_eq!(override_fork["agents_path"], expected);
        let no_change = req(Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.into()),
            agents: Some("role.md".into()),
            ..Default::default()
        })));
        store.execute(no_change, None).await.unwrap();
        let count:i64=store.database.call(|db|Ok(db.query_row("SELECT count(*) FROM event WHERE json_extract(payload_ref_json,'$.kind')='agents_reference'",[],|r|r.get(0))?)).await.unwrap();
        assert_eq!(
            count, 1,
            "same resolved reference must not fabricate events"
        );
        std::fs::remove_file(&expected).unwrap();
        assert_eq!(
            store.execute(request, None).await.unwrap().result,
            first,
            "replay must not revalidate a file removed after acceptance"
        );
        let before = details(&store, thread).await;
        let failed = store
            .execute(
                req(Command::ThreadSet(Box::new(ThreadSetArgs {
                    thread_id: Some(thread.into()),
                    agents: Some("missing".into()),
                    cwd: Some(root.path().to_str().unwrap().into()),
                    model: Some("must-not-commit".into()),
                    title: Some("not committed".into()),
                    ..Default::default()
                }))),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(failed.code, ErrorCode::InvalidArgument);
        assert_eq!(details(&store, thread).await, before);
        let root_default = store
            .execute(req(create(None, None, None)), None)
            .await
            .unwrap()
            .result;
        assert_eq!(
            root_default["agents_path"],
            root.path().join("AGENTS.md").to_str().unwrap()
        );
        for (name, bytes, code) in [
            ("invalid", vec![255], ErrorCode::IoFailed),
            (
                "oversize",
                vec![b'x'; 8 * 1024 * 1024 + 1],
                ErrorCode::InputLimit,
            ),
        ] {
            std::fs::write(root.path().join(name), bytes).unwrap();
            assert_eq!(
                store
                    .execute(req(create(Some(name), None, None)), None)
                    .await
                    .unwrap_err()
                    .code,
                code
            );
        }
        std::fs::write(root.path().join("exact"), vec![b'x'; 8 * 1024 * 1024]).unwrap();
        store
            .execute(req(create(Some("exact"), None, None)), None)
            .await
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = root.path().join("unreadable");
            std::fs::write(&path, "private fixture").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
            // Privileged runners can bypass mode bits; do not claim denial there.
            if std::fs::File::open(&path).is_err() {
                assert_eq!(
                    store
                        .execute(req(create(Some("unreadable"), None, None)), None)
                        .await
                        .unwrap_err()
                        .code,
                    ErrorCode::IoFailed
                );
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            store
                .execute(req(create(Some("project"), None, None)), None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::IoFailed
        );
        assert_eq!(
            store
                .execute(req(create(Some(""), None, None)), None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }

    #[tokio::test]
    async fn child_and_control_fork_can_override_with_a_distinct_reference() {
        let (root, store) = fixture().await;
        std::fs::write(root.path().join("parent.md"), "parent role").unwrap();
        std::fs::write(root.path().join("child.md"), "child role").unwrap();
        let parent = store
            .execute(req(create(Some("parent.md"), None, None)), None)
            .await
            .unwrap()
            .result;
        let thread = parent["thread_id"].as_str().unwrap().to_owned();
        let child = store
            .execute(
                req(create(Some("child.md"), None, Some(thread.clone()))),
                None,
            )
            .await
            .unwrap()
            .result;
        assert_ne!(child["agents_path"], parent["agents_path"]);
        assert_eq!(
            child["agents_path"],
            root.path().join("child.md").to_str().unwrap()
        );
        let target = thread.clone();
        store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                history::append_node(
                    &tx,
                    &target,
                    "source",
                    None,
                    "user",
                    &json!({"text":"fork source"}),
                    "now",
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let fork = store
            .execute(
                req(Command::ThreadFork(ThreadForkArgs {
                    agents: Some("child.md".into()),
                    thread_id: thread,
                    node_id: "source".into(),
                    cwd: None,
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert_eq!(fork["agents_path"], child["agents_path"]);
        assert_ne!(
            fork["agents_path"], parent["agents_path"],
            "Control must keep the verified override even though History rejects raw overrides"
        );
    }

    #[tokio::test]
    async fn idle_agents_set_only_publishes_metadata_without_work_effects() {
        let (root, store) = fixture().await;
        std::fs::write(root.path().join("role.md"), "role").unwrap();
        let created = store
            .execute(req(create(None, None, None)), None)
            .await
            .unwrap()
            .result;
        let thread = created["thread_id"].as_str().unwrap().to_owned();
        let result = store
            .execute(
                req(Command::ThreadSet(Box::new(ThreadSetArgs {
                    thread_id: Some(thread.clone()),
                    agents: Some("role.md".into()),
                    ..Default::default()
                }))),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.changed_threads, vec![thread.clone()]);
        assert!(result.wake_threads.is_empty());
        assert!(result.quiet_wake_threads.is_empty());
        assert!(result.stop_runs.is_empty());
        assert!(result.compact_threads.is_empty());
        // No effect returned to Control can schedule a later driver/retry; this
        // guards the asynchronous path that a single immediate DB read cannot.
        store.database.call(move |db| {
            assert_eq!(db.query_row("SELECT count(*) FROM run WHERE thread_id=?1",[&thread],|r|r.get::<_,i64>(0))?,0);
            assert_eq!(db.query_row("SELECT count(*) FROM turn WHERE thread_id=?1",[&thread],|r|r.get::<_,i64>(0))?,0);
            assert_eq!(db.query_row("SELECT count(*) FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.thread_id=?1",[&thread],|r|r.get::<_,i64>(0))?,0);
            let event:String=db.query_row("SELECT id FROM event WHERE target_thread_id=?1 AND json_extract(payload_ref_json,'$.kind')='agents_reference'",[thread],|r|r.get(0))?;
            assert_eq!(db.query_row("SELECT count(*) FROM delivery WHERE event_id=?1",[&event],|r|r.get::<_,i64>(0))?,0);
            assert_eq!(db.query_row("SELECT count(*) FROM node WHERE id=?1",[format!("event-{event}")],|r|r.get::<_,i64>(0))?,0);
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn agents_preflight_cas_and_real_actor_are_atomic() {
        let (root, store) = fixture().await;
        std::fs::write(root.path().join("one.md"), "one").unwrap();
        std::fs::write(root.path().join("two.md"), "two").unwrap();
        let created = store
            .execute(req(create(Some("one.md"), None, None)), None)
            .await
            .unwrap()
            .result;
        let id = created["thread_id"].as_str().unwrap().to_owned();
        let request = req(Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(id.clone()),
            agents: Some("two.md".into()),
            title: Some("must-not-commit".into()),
            ..Default::default()
        })));
        let plan = store
            .prepare_agents(&request, &None)
            .await
            .unwrap()
            .unwrap();
        store
            .execute(
                req(Command::ThreadSet(Box::new(ThreadSetArgs {
                    thread_id: Some(id.clone()),
                    model: Some("concurrent".into()),
                    ..Default::default()
                }))),
                None,
            )
            .await
            .unwrap();
        let saved = details(&store, &id).await;
        let cloned = store.clone();
        let conflict = store
            .database
            .call(move |db| {
                Ok(cloned.execute_transaction(
                    db,
                    &request,
                    None,
                    Some(&plan),
                    &std::sync::OnceLock::new(),
                    &mut false,
                ))
            })
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(conflict.code, ErrorCode::OperationConflict);
        assert_eq!(details(&store, &id).await, saved);
        let origin = Origin {
            instance_id: "fixture".into(),
            thread_id: "actual-model".into(),
            run_id: "actual-run".into(),
            turn_id: "actual-turn".into(),
            execution_id: "actual-shell".into(),
            token: "private-token".into(),
        };
        store
            .execute(
                req(Command::ThreadSet(Box::new(ThreadSetArgs {
                    thread_id: Some(id.clone()),
                    agents: Some("two.md".into()),
                    ..Default::default()
                }))),
                Some(origin),
            )
            .await
            .unwrap();
        let target = id.clone();
        let events:Vec<String>=store.database.call(move|db|Ok(db.prepare("SELECT payload_ref_json FROM event WHERE target_thread_id=?1 AND json_extract(payload_ref_json,'$.kind')='agents_reference' ORDER BY rowid")?.query_map([target],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?)).await.unwrap();
        let event: Value = serde_json::from_str(events.last().unwrap()).unwrap();
        assert_eq!(event["actor"]["thread_id"], "actual-model");
        assert_eq!(event["actor"]["turn_id"], "actual-turn");
        assert!(event["observed_at"].is_string());
        assert!(!events.join("").contains("private-token"));
        assert_eq!(
            event["change"]["before"]["path"],
            root.path().join("one.md").to_str().unwrap()
        );
        assert_eq!(
            event["change"]["after"]["path"],
            root.path().join("two.md").to_str().unwrap()
        );
        store
            .execute(
                req(Command::ThreadSet(Box::new(ThreadSetArgs {
                    thread_id: Some(id.clone()),
                    cwd: Some("..".into()),
                    ..Default::default()
                }))),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            details(&store, &id).await["agents_path"],
            event["change"]["after"]["path"]
        );
    }
    #[tokio::test]
    async fn sender_receipts_cover_appends_deleted_sources_labels_and_cancel_authority() {
        let (_root, store) = fixture().await;
        let source = store
            .execute(req(create(None, None, None)), None)
            .await
            .unwrap()
            .result["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let target = store
            .execute(req(create(None, None, None)), None)
            .await
            .unwrap()
            .result["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let origin = Origin {
            instance_id: "fixture".into(),
            thread_id: source.clone(),
            run_id: "source-run".into(),
            turn_id: "source-turn".into(),
            execution_id: "source-shell".into(),
            token: "secret-never-persist-or-return".into(),
        };
        let mut prior_run = None;
        let mut model_id = String::new();
        for (caller, label, kind) in [
            (None, None, "external_cli"),
            (None, Some("other owner"), "external_cli"),
            (Some(origin.clone()), Some("human"), "model"),
        ] {
            let accepted = store
                .execute(
                    req(Command::MessageSend(MessageSendArgs {
                        human: false,
                        resume_recovery: false,
                        to_human: false,
                        expect_reply: false,
                        thread_id: Some(target.clone()),
                        text: "synthetic sender fixture".into(),
                        deliver: Deliver::Next,
                        sender_label: label.map(str::to_owned),
                    })),
                    caller,
                )
                .await
                .unwrap()
                .result;
            if let Some(run) = &prior_run {
                assert_eq!(&accepted["run_id"], run);
                assert_eq!(accepted["assignment"], "existing_turn");
            }
            prior_run = Some(accepted["run_id"].clone());
            let id = accepted["message_id"].as_str().unwrap().to_owned();
            let expected = json!({"kind":kind,"label":label});
            let result = store
                .execute(
                    req(Command::MessageGet(MessageTarget {
                        message_id: id.clone(),
                    })),
                    None,
                )
                .await
                .unwrap()
                .result;
            assert_eq!(result["sender"]["kind"], expected["kind"]);
            assert_eq!(result["sender"]["label"], expected["label"]);
            let node = id.clone();
            let facts = store
                .database
                .call(move |db| {
                    let body = history::get_node(
                        db,
                        &HistoryGetArgs {
                            end_message_only: false,
                            parts: false,
                            node_id: node.clone(),
                            limit_bytes: MAX_BODY_BYTES,
                            cursor: None,
                            encoding: DEFAULT_ENCODING.into(),
                        },
                    )
                    .unwrap();
                    let raw: String = db.query_row(
                        "SELECT sender_json FROM receipt WHERE message_node_id=?1",
                        [&node],
                        |row| row.get(0),
                    )?;
                    Ok((serde_json::to_value(body).unwrap(), raw))
                })
                .await
                .unwrap();
            assert_eq!(facts.0["provenance"]["sender"], result["sender"]);
            assert!(!format!("{result}{facts:?}").contains(&origin.token));
            if kind == "model" {
                assert_eq!(result["sender"]["thread_id"], source);
                model_id = id;
            }
        }
        // Sender IDs are facts, not live Thread foreign-key lookups.
        let deleted = source.clone();
        store
            .database
            .call(move |db| {
                db.execute(
                    "UPDATE thread SET deleted_at='fixture-deleted' WHERE id=?1",
                    [deleted],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let result = store
            .execute(
                req(Command::MessageGet(MessageTarget {
                    message_id: model_id.clone(),
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert_eq!(result["sender"]["thread_id"], source);
        let own = store
            .execute(
                req(Command::MessageCancel(MessageCancelArgs {
                    message_ids: vec![model_id.clone()],
                })),
                Some(origin.clone()),
            )
            .await
            .unwrap()
            .result;
        assert_eq!(
            own["items"][0]["outcome"], "cancelled",
            "additive kind/label must preserve top-level sender ownership"
        );
        let mut foreign = origin.clone();
        foreign.thread_id = "different-thread".into();
        let denied = store
            .execute(
                req(Command::MessageCancel(MessageCancelArgs {
                    message_ids: vec![model_id.clone()],
                })),
                Some(foreign),
            )
            .await
            .unwrap()
            .result;
        assert_eq!(denied["items"][0]["outcome"], "forbidden");
        // A model claiming 'human' still cannot resume a recovery-wait Turn.
        let target_id = target.clone();
        store
            .database
            .call(move |db| {
                db.execute(
                    "UPDATE turn SET state='recovery_wait' WHERE thread_id=?1",
                    [target_id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let held = store
            .execute(
                req(Command::MessageSend(MessageSendArgs {
                    human: false,
                    resume_recovery: false,
                    to_human: false,
                    expect_reply: false,
                    thread_id: Some(target),
                    text: "no authority from label".into(),
                    deliver: Deliver::Next,
                    sender_label: Some("human".into()),
                })),
                Some(origin),
            )
            .await
            .unwrap()
            .result;
        assert_eq!(held["delivery_status"], "held");
        store.database.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn sender_human_is_recorded_only_by_internal_submission_and_history_lists_it() {
        let (_root, store) = fixture().await;
        let accepted = store
            .send_new_thread(
                format!("fixture:{}", uuid::Uuid::new_v4()),
                ThreadCreateArgs {
                    agents: None,
                    connection: None,
                    model: None,
                    effort: None,
                    cwd: None,
                    parent_id: None,
                },
                "TUI synthetic input".into(),
            )
            .await
            .unwrap()
            .result;
        let id = accepted["message_id"].as_str().unwrap().to_owned();
        let thread = accepted["thread_id"].as_str().unwrap().to_owned();
        let detail = store
            .execute(
                req(Command::MessageGet(MessageTarget {
                    message_id: id.clone(),
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert_eq!(detail["sender"], json!({"kind":"human"}));
        store
            .database
            .call(move |db| {
                // Source-backed selected-path projection, independent of a runtime.
                db.execute(
                    "INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES(?1,0,?2)",
                    params![thread, id],
                )?;
                let args: HistoryListArgs =
                    serde_json::from_value(json!({"thread_id":thread})).unwrap();
                let list = history::list_nodes(db, &args).unwrap();
                assert_eq!(
                    list.items[0].provenance.sender.as_ref().unwrap().kind,
                    SenderKind::Human
                );
                db.execute(
                    "UPDATE receipt SET sender_json='null' WHERE message_node_id=?1",
                    [&id],
                )?;
                assert_eq!(
                    message_details(db, &id).unwrap().sender.kind,
                    SenderKind::Unknown
                );
                assert_eq!(
                    history::provenance(db, &id).unwrap().sender.unwrap().kind,
                    SenderKind::Unknown
                );
                Ok(())
            })
            .await
            .unwrap();
        store.database.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod interrupted_message_assignment_tests {
    use super::*;
    #[test]
    fn only_human_new_run_recovers_original_assigned_input() {
        for caller_kind in ["human", "parent", "foreign"] {
            let mut db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','/','/a','p','m','0'),('foreign','/','/a','p','m','0'),('child','/','/a','p','m','0'); UPDATE thread SET parent_thread_id='parent' WHERE id='child'; INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent','live','0'),('foreign-run','foreign','live','0'),('old','child','stopped','0'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('old-turn','child','old','stopped','0'); INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('input','old-turn','user','0','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at)VALUES('input','operation','null','child','input','old','old-turn','held','0');").unwrap();
            let caller = (caller_kind != "human").then(|| Origin {
                instance_id: "fixture".into(),
                thread_id: caller_kind.into(),
                run_id: format!("{caller_kind}-run"),
                turn_id: "caller-turn".into(),
                execution_id: "shell".into(),
                token: "private-origin-token".into(),
            });
            let tx = db.transaction().unwrap();
            let assigned = assign_message(
                &tx,
                "child",
                caller.as_ref(),
                Deliver::Next,
                true,
                false,
                "resume",
            )
            .unwrap();
            assert_eq!(assigned.assignment, Assignment::NewRun);
            let count: i64 = tx
                .query_row(
                    "SELECT count(*) FROM thread_path WHERE thread_id='child' AND node_id='input'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                count,
                if caller_kind == "human" { 1 } else { 0 },
                "{caller_kind}"
            );
            let original: (String, String) = tx
                .query_row(
                    "SELECT run_id,turn_id FROM receipt WHERE id='input'",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .unwrap();
            assert_eq!(original, ("old".into(), "old-turn".into()));
            let payloads: Vec<String> = tx
                .prepare("SELECT payload_ref_json FROM event")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert!(!payloads.join("").contains("private-origin-token"));
            tx.commit().unwrap();
        }
    }
}

#[cfg(test)]
mod draft_recovery_revision_tests {
    use super::*;
    #[tokio::test]
    async fn actual_draft_resume_preserves_original_cas_and_registers_zero_or_one_file_after_recovery()
     {
        for file_count in [0, 1] {
            let root = tempfile::tempdir().unwrap();
            let db = DbWorker::open(root.path().join("draft.db")).await.unwrap();
            let config=Config::parse("default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
            let store = Store::new(
                db.clone(),
                Arc::new(config),
                root.path().into(),
                "fixture".into(),
            );
            let cwd = root.path().to_string_lossy().into_owned();
            db.call(move |db| {
                db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t',?1,?1,'fixture','synthetic','0')",[cwd])?;
                db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('old','t','stopped','0'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('old-turn','t','old','stopped','0'); INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('old-input','old-turn','user','0','{\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"earlier\"}]}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at)VALUES('old-receipt','old-op','null','t','old-input','old','old-turn','held','0');")?;
                Ok(())
            }).await.unwrap();
            let file = root.path().join("synthetic.txt");
            std::fs::write(&file, b"fixture").unwrap();
            let files = if file_count == 0 {
                vec![]
            } else {
                vec![crate::attachments::PlannedAttachment {
                    path: file,
                    kind: AttachmentKind::File,
                    observed_bytes: Some(7),
                    managed_id: None,
                }]
            };
            let args = ThreadCreateArgs {
                agents: None,
                cwd: None,
                parent_id: None,
                connection: None,
                model: None,
                effort: None,
            };
            let rejected = store
                .send_draft_content(
                    "fixture:stale-draft".into(),
                    args.clone(),
                    Some("t".into()),
                    crate::attachments::DraftContent::appended("new".into(), file_count),
                    files.clone(),
                    DraftSendOptions {
                        expected_selection_revision: Some(99),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_err();
            assert_eq!(rejected.code, ErrorCode::InvalidState);
            db.call(|db| {
                assert_eq!(
                    db.query_row("SELECT count(*) FROM run WHERE state='live'", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM thread_path", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                assert_eq!(
                    db.query_row(
                        "SELECT state FROM receipt WHERE id='old-receipt'",
                        [],
                        |r| r.get::<_, String>(0)
                    )?,
                    "held"
                );
                Ok(())
            })
            .await
            .unwrap();
            let accepted = store
                .send_draft_content(
                    "fixture:resume-draft".into(),
                    args,
                    Some("t".into()),
                    crate::attachments::DraftContent::appended("new".into(), file_count),
                    files,
                    DraftSendOptions {
                        expected_selection_revision: Some(0),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let new_node = accepted.result["message_id"].as_str().unwrap().to_owned();
            db.call(move |db| {
                assert_eq!(db.query_row("SELECT node_id FROM thread_path WHERE ordinal=0",[],|r|r.get::<_,String>(0))?,"old-input");
                assert_eq!(db.query_row("SELECT run_id,turn_id,state,path_admission FROM receipt WHERE id='old-receipt'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?,("old".into(),"old-turn".into(),"applied".into(),"admitted".into()));
                assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE message_node_id=?1 AND state='accepted'",[&new_node],|r|r.get::<_,i64>(0))?,1);
                assert_eq!(db.query_row("SELECT count(*) FROM attachment_ref WHERE source_node_id=?1",[new_node],|r|r.get::<_,i64>(0))?,file_count as i64);
                Ok(())
            }).await.unwrap();
            db.shutdown().await.unwrap();
        }
    }
}

#[cfg(test)]
mod message_acknowledgement_state_tests {
    use super::*;
    #[tokio::test]
    async fn typed_immediate_ack_replays_original_while_get_preserves_assignment_after_cancellation_metadata()
     {
        let root = tempfile::tempdir().unwrap();
        let db = DbWorker::open(root.path().join("ack.db")).await.unwrap();
        db.call(|db| {db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/','fixture','synthetic','0')",[])?;Ok(())}).await.unwrap();
        let store = Store::new(
            db.clone(),
            Arc::new(Config::default()),
            root.path().into(),
            "fixture".into(),
        );
        let request = Request::new(
            "fixture:typed-ack",
            Command::MessageSend(MessageSendArgs {
                human: false,
                resume_recovery: false,
                to_human: false,
                expect_reply: false,
                sender_label: None,
                thread_id: Some("t".into()),
                text: "urgent".into(),
                deliver: Deliver::Now,
            }),
        );
        let first = store.execute(request.clone(), None).await.unwrap();
        let accepted: MessageAccepted = serde_json::from_value(first.result.clone()).unwrap();
        assert_eq!(accepted.deliver, Deliver::Now);
        assert_eq!(accepted.assignment, Assignment::NewRun);
        store
            .execute(
                Request::new(
                    "fixture:cancel-ack",
                    Command::MessageCancel(MessageCancelArgs {
                        message_ids: vec![accepted.message_id.clone()],
                    }),
                ),
                None,
            )
            .await
            .unwrap();
        let current = store
            .execute(
                Request::new(
                    "fixture:get-ack",
                    Command::MessageGet(MessageTarget {
                        message_id: accepted.message_id.clone(),
                    }),
                ),
                None,
            )
            .await
            .unwrap();
        let current: MessageDetails = serde_json::from_value(current.result).unwrap();
        assert_eq!(current.assignment, Assignment::NewRun);
        assert_eq!(current.run_id, accepted.run_id);
        assert_eq!(current.turn_id, accepted.turn_id);
        assert_eq!(current.delivery_status, DeliveryStatus::Cancelled);
        let replay = store.execute(request, None).await.unwrap();
        assert_eq!(replay.result, first.result);
        assert!(replay.wake_threads.is_empty() && replay.quiet_wake_threads.is_empty());
        let replay: MessageAccepted = serde_json::from_value(replay.result).unwrap();
        assert_eq!(replay.deliver, Deliver::Now);
        assert_eq!(replay.delivery_status, DeliveryStatus::Accepted);
        db.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod thread_deletion_notification_tests {
    use super::*;
    use crate::control::EngineEvent;
    use crate::{app::DataOwner, control::Control};
    use std::collections::BTreeSet;

    async fn fixture() -> (tempfile::TempDir, DataOwner) {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        (root, owner)
    }

    async fn seed(db: &DbWorker) {
        db.call(|db| {
            for (id, parent) in [("parent", None), ("child", Some("parent")), ("grandchild", Some("child")), ("unrelated", None)] {
                db.execute("INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,?1,'/','','fixture','synthetic','now')", params![id,parent])?;
            }
            Ok(())
        }).await.unwrap();
    }

    fn delete(instance: &str, descendants: bool) -> Request {
        Request::new(
            format!("{instance}:delete-notification"),
            Command::ThreadDelete(ThreadDeleteArgs {
                preview_marker: None,
                thread_id: "parent".into(),
                descendants,
            }),
        )
    }

    #[tokio::test]
    async fn deletion_effects_are_exact_committed_targets_and_replay_has_no_effects() {
        for descendants in [false, true] {
            let (root, owner) = fixture().await;
            seed(&owner.db).await;
            let store = Store::new(
                owner.db.clone(),
                Arc::new(owner.loaded.config.clone()),
                root.path().into(),
                owner.instance_id.clone(),
            );
            let request = delete(&owner.instance_id, descendants);
            let first = store.execute(request.clone(), None).await.unwrap();
            let targets = first
                .changed_threads
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            let expected = if descendants {
                BTreeSet::from([
                    "parent".to_owned(),
                    "child".to_owned(),
                    "grandchild".to_owned(),
                ])
            } else {
                BTreeSet::from(["parent".to_owned()])
            };
            assert_eq!(targets, expected);
            assert_eq!(first.changed_threads.len(), targets.len());
            assert!(
                first.wake_threads.is_empty()
                    && first.quiet_wake_threads.is_empty()
                    && first.stop_runs.is_empty()
                    && first.compact_threads.is_empty()
            );
            let replay = store.execute(request, None).await.unwrap();
            assert_eq!(replay.result, first.result);
            assert!(
                replay.changed_threads.is_empty()
                    && replay.wake_threads.is_empty()
                    && replay.quiet_wake_threads.is_empty()
            );
            owner.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn deleting_thread_handles_empty_and_legacy_over_cap_reply_sets() {
        for count in [0, 17, 33, 129, 1024] {
            let (root, owner) = fixture().await;
            seed(&owner.db).await;
            if count > 0 {
                owner.db.call(move |db| {
                    for index in 0..count {
                        let message = format!("legacy-{index}");
                        db.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES(?1,'user','old','{}')", [&message])?;
                        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES(?1,?1,'{\"kind\":\"human\"}','parent',?1,'applied',datetime('now'),'{\"reply\":{\"state\":\"pending\"}}')", [&message])?;
                    }
                    Ok(())
                }).await.unwrap();
            }
            let store = Store::new(
                owner.db.clone(),
                Arc::new(owner.loaded.config.clone()),
                root.path().into(),
                owner.instance_id.clone(),
            );
            store
                .execute(delete(&owner.instance_id, false), None)
                .await
                .unwrap();
            let states = owner.db.call(|db| Ok(db.query_row(
                "SELECT count(*),sum(json_extract(result_json,'$.reply.state')='recipient_deleted') FROM receipt WHERE target_thread_id='parent'",
                [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
            )?)).await.unwrap();
            let synchronous =
                count.min(crate::protocol::MAX_SYNC_DELETION_REPLY_DUTIES as usize) as i64;
            assert_eq!(states, (count as i64, synchronous));
            let durable_job = owner
                .db
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT count(*) FROM gc_job WHERE kind='history' AND state='pending'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )?)
                })
                .await
                .unwrap();
            assert!(
                durable_job > 0,
                "delete commits durable history continuation for {count} legacy receipts"
            );
            owner.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn thread_delete_history_job_resumes_after_owner_reopen() {
        let (root, owner) = fixture().await;
        seed(&owner.db).await;
        owner.db.call(|db| {
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('sender','sender','/','','fixture','synthetic','now')", [])?;
            db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('sender-run','sender','live','now')", [])?;
            let sender = json!({"kind":"model","instance_id":"fixture","thread_id":"sender","run_id":"sender-run","turn_id":"sender-turn","execution_id":"sender-shell"}).to_string();
            for n in 0..136 {
                let id = format!("owner-message-{n:03}");
                db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'user','now','{}',2)", [&id])?;
                db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,path_admission,created_at,result_json)VALUES(?1,?1,?2,?3,?1,'held','unadmitted','now','{\"expect_reply\":true,\"reply\":{\"state\":\"pending\"}}')", params![id,sender,if n < 129 {"parent"} else {"unrelated"}])?;
            }
            // Audit rows participate in the same transactions as the production
            // writes. A failed batch cannot leave audit or outcome fragments.
            db.execute_batch(r#"
                CREATE TABLE fixture_reply_phase(phase TEXT NOT NULL);
                INSERT INTO fixture_reply_phase VALUES('sync');
                CREATE TABLE fixture_reply_transition(message TEXT NOT NULL,phase TEXT NOT NULL,batch INTEGER);
                CREATE TABLE fixture_reply_batch(id INTEGER PRIMARY KEY,scanned INTEGER NOT NULL,terminalized INTEGER NOT NULL);
                CREATE TABLE fixture_reply_delivery(event TEXT NOT NULL,run TEXT NOT NULL);
                CREATE TRIGGER fixture_reply_delivery AFTER INSERT ON delivery
                WHEN NEW.event_id GLOB 'reply-outcome-owner-message-*'
                BEGIN INSERT INTO fixture_reply_delivery VALUES(NEW.event_id,NEW.recipient_id); END;
                CREATE TRIGGER fixture_reply_transition AFTER UPDATE OF result_json ON receipt
                WHEN json_extract(OLD.result_json,'$.reply.state')='pending'
                 AND json_extract(NEW.result_json,'$.reply.state')='recipient_deleted'
                BEGIN
                    INSERT INTO fixture_reply_transition VALUES(NEW.message_node_id,(SELECT phase FROM fixture_reply_phase),NULL);
                END;
                CREATE TRIGGER fixture_reply_batch AFTER UPDATE OF cursor_json ON gc_job
                WHEN OLD.kind='history' AND json_extract(OLD.cursor_json,'$.stage')='receipts'
                 AND json_extract(NEW.cursor_json,'$.stage') IN ('receipts','roots')
                BEGIN
                    INSERT INTO fixture_reply_batch(scanned,terminalized)
                    SELECT count(*),(SELECT count(*) FROM fixture_reply_transition WHERE phase='gc' AND batch IS NULL)
                    FROM receipt WHERE rowid>json_extract(OLD.cursor_json,'$.last_rowid')
                     AND rowid<=CASE WHEN json_extract(NEW.cursor_json,'$.stage')='receipts'
                         THEN json_extract(NEW.cursor_json,'$.last_rowid') ELSE json_extract(OLD.cursor_json,'$.receipt_horizon') END;
                    UPDATE fixture_reply_transition SET batch=last_insert_rowid() WHERE phase='gc' AND batch IS NULL;
                END;
                CREATE TRIGGER fixture_reply_fault BEFORE INSERT ON event
                WHEN NEW.id='reply-outcome-owner-message-005'
                BEGIN SELECT RAISE(ABORT,'fixture synchronous outcome rollback'); END;
            "#)?;
            Ok(())
        }).await.unwrap();
        let store = Store::new(
            owner.db.clone(),
            Arc::new(owner.loaded.config.clone()),
            root.path().into(),
            owner.instance_id.clone(),
        );
        let failed = Request::new(
            format!("{}:delete-rollback", owner.instance_id),
            delete(&owner.instance_id, false).command,
        );
        assert!(store.execute(failed, None).await.is_err());
        owner
            .db
            .call(|db| {
                assert_eq!(
                    db.query_row("SELECT count(*) FROM fixture_reply_transition", [], |r| r
                        .get::<_, i64>(
                        0
                    ))?,
                    0
                );
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM event WHERE kind='reply_outcome'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                assert!(db.query_row(
                    "SELECT deleted_at IS NULL FROM thread WHERE id='parent'",
                    [],
                    |r| r.get::<_, bool>(0),
                )?);
                db.execute_batch("DROP TRIGGER fixture_reply_fault")?;
                Ok(())
            })
            .await
            .unwrap();
        store
            .execute(delete(&owner.instance_id, false), None)
            .await
            .unwrap();
        let job = owner
            .db
            .call(|db| {
                Ok(db.query_row(
                    "SELECT id FROM gc_job WHERE kind='history' AND state='pending'",
                    [],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap();
        owner.db.call(|db| {
            let count: i64 = db.query_row("SELECT count(*) FROM fixture_reply_transition WHERE phase='sync'", [], |r| r.get(0))?;
            assert_eq!(count, 16, "measure the deletion transaction, not a shutdown snapshot");
            assert_eq!(db.query_row("SELECT count(*) FROM event WHERE kind='reply_outcome'", [], |r| r.get::<_,i64>(0))?, count);
            db.execute_batch("UPDATE fixture_reply_phase SET phase='gc'; CREATE TRIGGER fixture_reply_fault BEFORE INSERT ON event WHEN NEW.id='reply-outcome-owner-message-020' BEGIN SELECT RAISE(ABORT,'fixture GC outcome rollback'); END;")?;
            Ok(())
        }).await.unwrap();
        // Before starting the owner worker, drive the real bounded service to
        // one failed and one committed receipt batch, then close at that boundary.
        let attachments = Arc::new(crate::attachments::AttachmentService::new(
            owner.db.clone(),
            &owner.loaded.config,
            root.path().into(),
            owner.instance_id.clone(),
        ));
        let gc = crate::gc::GcService::new(
            owner.db.clone(),
            Arc::new(owner.loaded.config.clone()),
            root.path().into(),
            attachments,
        );
        let resumed = gc.start_history().await.unwrap().unwrap();
        assert_eq!(resumed, job);
        let mut rollback = false;
        for _ in 0..40 {
            match gc.history_step(&job).await {
                Ok(step) => assert!(step.candidates <= 128),
                Err(_) => {
                    rollback = true;
                    break;
                }
            }
        }
        assert!(
            rollback,
            "fault must interrupt a receipt batch after partial work"
        );
        owner
            .db
            .call(|db| {
                assert_eq!(
                    db.query_row("SELECT count(*) FROM fixture_reply_transition", [], |r| r
                        .get::<_, i64>(
                        0
                    ))?,
                    16
                );
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM event WHERE kind='reply_outcome'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    16
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM fixture_reply_batch", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                db.execute_batch("DROP TRIGGER fixture_reply_fault")?;
                Ok(())
            })
            .await
            .unwrap();
        let batch = gc.history_step(&job).await.unwrap();
        assert_eq!(batch.candidates, 128);
        owner.db.call(|db| {
            assert_eq!(db.query_row("SELECT scanned,terminalized FROM fixture_reply_batch", [], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?)))?, (128,112));
            assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE target_thread_id='parent' AND json_extract(result_json,'$.reply.state')='pending'", [], |r| r.get::<_,i64>(0))?, 1);
            Ok(())
        }).await.unwrap();
        drop(gc);
        drop(store);
        let control = Control::new(&owner, root.path().into());
        // No maintenance was started here: shutdown's all-live-Run sweep is
        // tested separately from the deterministic deletion/batch boundaries.
        control.shutdown().await.unwrap();
        owner
            .db
            .call(|db| {
                assert_eq!(
                    db.query_row("SELECT state FROM run WHERE id='sender-run'", [], |r| {
                        r.get::<_, String>(0)
                    })?,
                    "stopped"
                );
                Ok(())
            })
            .await
            .unwrap();
        drop(control);
        owner.shutdown().await.unwrap();

        let reopened = DataOwner::start(
            Some(root.path().join("config.toml")),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&reopened, root.path().into());
        control.start().await.unwrap();
        crate::test_watchdog::watchdog(
            "reopened existing history worker drains bounded continuation",
            async {
                loop {
                    let job = job.clone();
                    let done = reopened
                        .db
                        .call(move |db| {
                            Ok(db.query_row(
                                "SELECT state='done' FROM gc_job WHERE id=?1",
                                [job],
                                |r| r.get::<_, bool>(0),
                            )?)
                        })
                        .await
                        .unwrap();
                    if done {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            },
        )
        .await
        .expect("test watchdog elapsed while waiting for the durable GC continuation");
        control.shutdown().await.unwrap();
        reopened.db.call(|db| {
            let mut statement = db.prepare("SELECT scanned,terminalized FROM fixture_reply_batch ORDER BY id")?;
            let batches = statement.query_map([], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            assert!(batches.len() >= 2, "receipt work must span commits and reopen");
            assert!(batches.iter().all(|&(scanned,terminalized)| scanned <= 128 && terminalized <= scanned));
            assert_eq!(db.query_row("SELECT count(*) FROM fixture_reply_transition WHERE phase='sync'", [], |r| r.get::<_,i64>(0))?, 16);
            assert_eq!(db.query_row("SELECT count(*),count(DISTINCT message) FROM fixture_reply_transition", [], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?)))?, (129,129));
            assert_eq!(db.query_row("SELECT count(*) FROM fixture_reply_transition WHERE phase='gc' AND batch IS NULL", [], |r| r.get::<_,i64>(0))?, 0);
            for n in 0..129 {
                let message = format!("owner-message-{n:03}");
                let event = format!("reply-outcome-{message}");
                assert_eq!(db.query_row("SELECT json_extract(result_json,'$.reply.state') FROM receipt WHERE id=?1", [&message], |r| r.get::<_,String>(0))?, "recipient_deleted");
                assert_eq!(db.query_row("SELECT count(*) FROM event e JOIN fixture_reply_delivery d ON d.event=e.id JOIN run r ON r.id=d.run WHERE e.id=?1 AND e.kind='reply_outcome' AND e.target_thread_id='sender' AND r.thread_id='sender' AND json_extract(e.payload_ref_json,'$.message_id')=?2 AND json_extract(e.payload_ref_json,'$.state')='recipient_deleted'", params![event,message], |r| r.get::<_,i64>(0))?, 1);
            }
            assert_eq!(db.query_row("SELECT count(*) FROM event WHERE kind='reply_outcome'", [], |r| r.get::<_,i64>(0))?, 129);
            assert_eq!(db.query_row("SELECT count(*) FROM fixture_reply_delivery d JOIN run r ON r.id=d.run WHERE d.event='reply-outcome-owner-message-128' AND r.thread_id='sender' AND r.id<>'sender-run'", [], |r| r.get::<_,i64>(0))?, 1, "reopened outcome uses a new independent sender Run");
            assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE target_thread_id='unrelated' AND result_json='{\"expect_reply\":true,\"reply\":{\"state\":\"pending\"}}'", [], |r| r.get::<_,i64>(0))?, 7);
            for n in 129..136 {
                assert_eq!(db.query_row("SELECT payload_json FROM node WHERE id=?1", [format!("owner-message-{n:03}")], |r| r.get::<_,String>(0))?, "{}");
            }
            eprintln!("audited sync=16; GC (scanned,terminalized)={batches:?}; unique outcomes/deliveries=129; unrelated receipts=7");
            Ok(())
        }).await.unwrap();
        reopened.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn deleting_a_thread_removes_its_automatic_compaction_metadata() {
        let (root, owner) = fixture().await;
        seed(&owner.db).await;
        let keys = |id: &str| {
            let keys = crate::compaction::CompactionKeys::new(id);
            [
                keys.automatic_active(),
                crate::compaction::CompactionKeys::new(id).automatic_target(),
                crate::compaction::CompactionKeys::new(id).automatic_guard(),
                crate::compaction::CompactionKeys::new(id).automatic_status(),
            ]
        };
        let seeded: Vec<String> = keys("parent").into_iter().chain(keys("child")).collect();
        let saved = seeded.clone();
        owner
            .db
            .call(move |db| {
                for key in &saved {
                    db.execute("INSERT INTO app_meta(key,value) VALUES(?1,'x')", [key])?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let store = Store::new(
            owner.db.clone(),
            Arc::new(owner.loaded.config.clone()),
            root.path().into(),
            owner.instance_id.clone(),
        );
        store
            .execute(delete(&owner.instance_id, false), None)
            .await
            .unwrap();
        let remaining: Vec<String> = owner.db.call(|db| Ok(db.prepare("SELECT key FROM app_meta WHERE key GLOB 'compact.automatic_*' ORDER BY key")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)).await.unwrap();
        let mut expected: Vec<String> = keys("child").into();
        expected.sort();
        assert_eq!(
            remaining, expected,
            "only the deleted Thread's metadata, including the witness, is removed"
        );
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn external_deletion_notifies_after_commit_but_never_on_replay_rejection_or_rollback() {
        for mode in 0..4 {
            let (root, owner) = fixture().await;
            let control = Control::new(&owner, root.path().into());
            control.start().await.unwrap();
            seed(&owner.db).await;
            owner.db.call(move |db| {
                if mode == 2 {
                    db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('live-child','child','live','now')", [])?;
                } else if mode == 3 {
                    db.execute_batch("CREATE TRIGGER reject_child_deletion BEFORE UPDATE OF deleted_at ON thread WHEN OLD.id='child' BEGIN SELECT RAISE(ABORT,'synthetic deletion rollback'); END;")?;
                }
                Ok(())
            }).await.unwrap();
            let mut events = control.subscribe();
            let request = delete(&owner.instance_id, mode != 0);
            let response = control.request(request.clone()).await;
            assert_eq!(response.ok, mode < 2, "mode={mode}, {response:?}");
            if mode >= 2 {
                assert_eq!(
                    response.error.as_ref().unwrap().code,
                    if mode == 2 {
                        ErrorCode::InvalidState
                    } else {
                        ErrorCode::SaveFailed
                    }
                );
            }
            let mut notified = BTreeSet::new();
            while let Ok(event) = events.try_recv() {
                let EngineEvent::Changed {
                    thread_id,
                    recovery: false,
                } = event
                else {
                    panic!("unexpected deletion event: {event:?}")
                };
                assert!(notified.insert(thread_id), "one notification per target");
            }
            let committed = owner
                .db
                .call(|db| {
                    let ids = db
                        .prepare("SELECT id FROM thread WHERE deleted_at IS NOT NULL")?
                        .query_map([], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
                    Ok(ids)
                })
                .await
                .unwrap();
            let expected = match mode {
                0 => BTreeSet::from(["parent".to_owned()]),
                1 => BTreeSet::from([
                    "parent".to_owned(),
                    "child".to_owned(),
                    "grandchild".to_owned(),
                ]),
                _ => BTreeSet::new(),
            };
            assert_eq!(notified, expected, "mode={mode}");
            assert_eq!(
                committed, notified,
                "event consumers see only committed deletion facts"
            );
            if mode < 2 {
                let replay = control.request(request).await;
                assert!(replay.ok);
                assert_eq!(replay.result, response.result);
                assert!(matches!(
                    events.try_recv(),
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                ));
            }
            let shutdown = control.shutdown().await;
            if mode == 3 {
                assert_eq!(shutdown.unwrap_err().code, ErrorCode::SaveFailed);
            } else {
                shutdown.unwrap();
            }
            owner.shutdown().await.unwrap();
        }
    }
}

#[cfg(test)]
mod independent_run_notification_tests {
    use super::*;
    use crate::{app::DataOwner, control::Control};
    use futures_util::FutureExt;

    fn parent_origin() -> Origin {
        Origin {
            instance_id: "fixture".into(),
            thread_id: "parent".into(),
            run_id: "parent-run".into(),
            turn_id: "parent-turn".into(),
            execution_id: "parent-shell".into(),
            token: "synthetic".into(),
        }
    }

    #[tokio::test]
    async fn new_run_acceptance_is_independent_and_does_not_notify_parent() {
        for mode in ["owned", "human", "foreign", "stopping-owner", "rollback"] {
            let root = tempfile::tempdir().unwrap();
            let db = DbWorker::open(root.path().join("fixture.db"))
                .await
                .unwrap();
            db.call(|db| {db.execute_batch("INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent',NULL,'parent','/','','p','m','now'),('child','parent','child','/','','p','m','now'),('foreign',NULL,'foreign','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent','live','now'),('foreign-run','foreign','live','now');")?;Ok(())}).await.unwrap();
            if mode == "stopping-owner" {
                db.call(|db| {
                    db.execute("UPDATE run SET state='stopping' WHERE id='parent-run'", [])?;
                    Ok(())
                })
                .await
                .unwrap();
            } else if mode == "rollback" {
                db.call(|db| {db.execute_batch("CREATE TRIGGER reject_child_receipt BEFORE INSERT ON receipt BEGIN SELECT RAISE(ABORT,'synthetic acceptance rollback'); END;")?;Ok(())}).await.unwrap();
            }
            let store = Store::new(
                db.clone(),
                Arc::new(Config::default()),
                root.path().into(),
                "fixture".into(),
            );
            let caller = match mode {
                "human" => None,
                "foreign" => Some(Origin {
                    thread_id: "foreign".into(),
                    run_id: "foreign-run".into(),
                    ..parent_origin()
                }),
                _ => Some(parent_origin()),
            };
            let request = Request::new(
                "fixture:new-child",
                Command::MessageSend(MessageSendArgs {
                    human: false,
                    resume_recovery: false,
                    to_human: false,
                    expect_reply: false,
                    sender_label: None,
                    thread_id: Some("child".into()),
                    text: "synthetic delegation".into(),
                    deliver: Deliver::Next,
                }),
            );
            let result = store.execute(request.clone(), caller.clone()).await;
            if mode == "rollback" {
                assert_eq!(result.unwrap_err().code, ErrorCode::SaveFailed);
                db.call(|db| {
                    assert_eq!(
                        db.query_row(
                            "SELECT count(*) FROM run WHERE thread_id='child'",
                            [],
                            |row| row.get::<_, i64>(0)
                        )?,
                        0
                    );
                    Ok(())
                })
                .await
                .unwrap();
            } else {
                let first = result.unwrap();
                assert!(first.changed_threads.is_empty());
                assert_eq!(first.quiet_wake_threads, vec!["child".to_owned()]);
                assert!(first.wake_threads.is_empty() && first.stop_runs.is_empty());
                db.call(|db| {assert_eq!(db.query_row("SELECT count(*) FROM pragma_table_info('run') WHERE name='owner_run_id'",[],|r|r.get::<_,i64>(0))?,0);Ok(())}).await.unwrap();
                let replay = store.execute(request, caller.clone()).await.unwrap();
                assert_eq!(replay.result, first.result);
                assert!(
                    replay.changed_threads.is_empty()
                        && replay.wake_threads.is_empty()
                        && replay.quiet_wake_threads.is_empty()
                );
                if mode == "owned" {
                    let second = store
                        .execute(
                            Request::new(
                                "fixture:existing-child",
                                Command::MessageSend(MessageSendArgs {
                                    human: false,
                                    resume_recovery: false,
                                    to_human: false,
                                    expect_reply: false,
                                    sender_label: None,
                                    thread_id: Some("child".into()),
                                    text: "another input".into(),
                                    deliver: Deliver::Next,
                                }),
                            ),
                            caller,
                        )
                        .await
                        .unwrap();
                    assert!(
                        second.changed_threads.is_empty(),
                        "an unchanged owner edge is not a new-child notification"
                    );
                }
            }
            db.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn saved_child_transitions_do_not_synthesize_parent_notifications() {
        for parent_state in ["live", "stopping", "completed"] {
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
            let state = parent_state.to_owned();
            owner.db.call(move |db| {
                db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','parent','/','','p','m','now'),('child','child','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent','live','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('child-run','child','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('child-turn','child','child-run','recovery_wait','now');")?;
                db.execute("UPDATE run SET state=?1 WHERE id='parent-run'",[state])?;Ok(())
            }).await.unwrap();
            // An existing synthetic driver captures the pre-existing quiet
            // nudge, so this test never starts a model HTTP request.
            let nudge = Arc::new(tokio::sync::Notify::new());
            control.live.test_drivers().lock().await.insert(
                "parent".into(),
                super::super::DriverSlot {
                    id: "synthetic-parent-driver".into(),
                    run_id: "parent-run".into(),
                    stop: tokio_util::sync::CancellationToken::new(),
                    nudge: nudge.clone(),
                    foreground: Arc::new(super::super::foreground::ForegroundSlot::default()),
                    turn: Arc::new(tokio::sync::Mutex::new(None)),
                },
            );
            let mut events = control.subscribe();
            for completed in [false, true] {
                if completed {
                    owner.db.call(|db| {db.execute_batch("UPDATE turn SET state='completed' WHERE id='child-turn'; UPDATE run SET state='completed' WHERE id='child-run';")?;Ok(())}).await.unwrap();
                }

                assert!(matches!(
                    events.try_recv(),
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                ));
                assert!(nudge.notified().now_or_never().is_none());
            }
            control.live.test_drivers().lock().await.clear();
            owner.shutdown().await.unwrap();
        }
    }
}

#[cfg(test)]
mod driverless_child_stop_notification_tests {
    use super::*;
    use crate::control::EngineEvent;
    use crate::{app::DataOwner, control::Control};
    use futures_util::FutureExt;

    #[tokio::test]
    async fn stopping_an_offscreen_run_never_refreshes_or_stops_unrelated_runs() {
        for (parent_state, fail_drain) in [
            ("live", false),
            ("stopping", false),
            ("completed", false),
            ("live", true),
        ] {
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
            control.start().await.unwrap();
            let parent_state_owned = parent_state.to_owned();
            owner.db.call(move |db| {
                let tx=db.transaction()?;
                tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','parent','/','','p','m','now')",[])?;
                tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent',?1,'2026-10-25T00:00:00Z')",[parent_state_owned])?;
                tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('parent-turn','parent','parent-run','completed','2026-10-25T00:00:00Z')",[])?;
                for index in 0..4 {
                    let thread=format!("child-{index}");let run=format!("run-{index}");let turn=format!("turn-{index}");
                    tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?1,'/','','p','m','now')",[&thread])?;
                    tx.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,?2,'live',?3)",params![run,thread,format!("2026-10-25T00:00:0{}Z",index+1)])?;
                    tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,'recovery_wait','2026-10-25T00:00:00Z')",params![turn,thread,run])?;
                }
                assert_eq!(tx.query_row("SELECT count(*) FROM run WHERE id IN ('run-0','run-1','run-2','run-3') AND state IN ('live','stopping')",[],|row|row.get::<_,i64>(0))?,4);
                if fail_drain {
                    tx.execute_batch("CREATE TRIGGER reject_child_drain BEFORE UPDATE OF state ON run WHEN OLD.id='run-3' AND NEW.state='stopped' BEGIN SELECT RAISE(ABORT,'synthetic stopped-child rollback'); END;")?;
                }
                tx.commit()?;Ok(())
            }).await.unwrap();
            // The selected parent is idle with a driver. Stopping its hidden
            // child must update metadata without starting/nudging model work.
            let nudge = Arc::new(tokio::sync::Notify::new());
            control.live.test_drivers().lock().await.insert(
                "parent".into(),
                super::super::DriverSlot {
                    id: "synthetic-idle-parent".into(),
                    run_id: "parent-run".into(),
                    stop: tokio_util::sync::CancellationToken::new(),
                    nudge: nudge.clone(),
                    foreground: Arc::new(super::super::foreground::ForegroundSlot::default()),
                    turn: Arc::new(tokio::sync::Mutex::new(None)),
                },
            );
            assert!(
                !control
                    .live
                    .test_drivers()
                    .lock()
                    .await
                    .contains_key("child-3")
            );
            let mut events = control.subscribe();
            let response = control
                .request(Request::new(
                    format!("{}:stop-hidden-child", owner.instance_id),
                    Command::RunStop(RunTarget {
                        run_id: "run-3".into(),
                    }),
                ))
                .await;
            assert!(
                response.ok,
                "the stop intent receipt commits independently of later drain: {response:?}"
            );
            let mut parent_notifications = 0;
            let mut child_notifications = 0;
            let mut errors = 0;
            while let Ok(event) = events.try_recv() {
                match event {
                    EngineEvent::Changed {
                        thread_id,
                        recovery: false,
                    } if thread_id == "parent" => parent_notifications += 1,
                    EngineEvent::Changed {
                        thread_id,
                        recovery: false,
                    } if thread_id == "child-3" => child_notifications += 1,
                    EngineEvent::Error { thread_id, error }
                        if fail_drain && thread_id == "child-3" =>
                    {
                        assert_eq!(error.code, ErrorCode::SaveFailed);
                        errors += 1;
                    }
                    other => panic!("unexpected stop notification: {other:?}"),
                }
            }
            assert_eq!(parent_notifications, 0);
            // One hint when the stopping state commits (#243) and one after the drain.
            assert_eq!(child_notifications, 2);
            assert_eq!(errors, usize::from(fail_drain));
            assert!(
                nudge.notified().now_or_never().is_none(),
                "display notification must never nudge the parent model"
            );
            owner.db.call(move |db| {
                let target=db.query_row("SELECT r.state,u.state FROM run r JOIN turn u ON u.run_id=r.id WHERE r.id='run-3'",[],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?;
                assert_eq!(target,if fail_drain {("stopping".into(),"recovery_wait".into())}else{("stopped".into(),"stopped".into())});
                assert_eq!(db.query_row("SELECT count(*) FROM run WHERE id IN ('run-0','run-1','run-2','run-3') AND state IN ('live','stopping')",[],|row|row.get::<_,i64>(0))?,if fail_drain {4}else{3});
                assert_eq!(db.query_row("SELECT count(*) FROM run WHERE id IN ('run-0','run-1','run-2') AND state='live'",[],|row|row.get::<_,i64>(0))?,3);
                if fail_drain {db.execute_batch("DROP TRIGGER reject_child_drain;")?;}
                Ok(())
            }).await.unwrap();
            control.live.test_drivers().lock().await.clear();
            let shutdown = control.shutdown().await;
            if fail_drain {
                assert_eq!(shutdown.unwrap_err().code, ErrorCode::SaveFailed);
            } else {
                shutdown.unwrap();
            }
            owner.shutdown().await.unwrap();
        }
    }
}

#[cfg(test)]
mod browser_creation_notification_tests {
    use super::*;
    use crate::control::EngineEvent;
    use crate::{app::DataOwner, control::Control};
    async fn fixture() -> (tempfile::TempDir, DataOwner, Arc<Control>) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(&path,"default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
        let owner = DataOwner::start(
            Some(path),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        control.start().await.unwrap();
        (root, owner, control)
    }
    fn create() -> Command {
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_id: None,
            connection: None,
            model: None,
            effort: None,
        })
    }
    #[tokio::test]
    async fn standalone_create_and_fork_publish_exact_postcommit_ids_without_replay_events() {
        let (_root, owner, control) = fixture().await;
        let mut events = control.subscribe();
        let create = Request::new(format!("{}:standalone-create", owner.instance_id), create());
        let first = control.request(create.clone()).await;
        assert!(first.ok, "{first:?}");
        let id = first.result.as_ref().unwrap()["thread_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(matches!(events.try_recv(),Ok(EngineEvent::Changed{thread_id,..})if thread_id==id));
        let replay = control.request(create).await;
        assert_eq!(replay.result, first.result);
        assert!(events.try_recv().is_err());
        let parent = id.clone();
        owner
            .db
            .call(move |db| {
                let tx = db.transaction()?;
                crate::history::append_node(
                    &tx,
                    &parent,
                    "fork-source",
                    None,
                    "user",
                    &json!({"role":"user","content":"source"}),
                    "now",
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let fork = Request::new(
            format!("{}:standalone-fork", owner.instance_id),
            Command::ThreadFork(ThreadForkArgs {
                agents: None,
                thread_id: id,
                node_id: "fork-source".into(),
                cwd: None,
            }),
        );
        let first = control.request(fork.clone()).await;
        assert!(first.ok, "{first:?}");
        let fork_id = first.result.as_ref().unwrap()["thread_id"]
            .as_str()
            .unwrap();
        assert!(
            matches!(events.try_recv(),Ok(EngineEvent::Changed{thread_id,..})if thread_id==fork_id)
        );
        let replay = control.request(fork).await;
        assert_eq!(replay.result, first.result);
        assert!(events.try_recv().is_err());
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn rolled_back_standalone_create_does_not_emit_or_retain_a_thread() {
        let (_root, owner, control) = fixture().await;
        owner.db.call(|db|{db.execute_batch("CREATE TRIGGER reject_browser_create BEFORE INSERT ON thread BEGIN SELECT RAISE(ABORT,'synthetic create rollback'); END;")?;Ok(())}).await.unwrap();
        let mut events = control.subscribe();
        let response = control
            .request(Request::new(
                format!("{}:rolled-back-create", owner.instance_id),
                create(),
            ))
            .await;
        assert!(!response.ok);
        assert!(events.try_recv().is_err());
        assert_eq!(
            owner
                .db
                .call(|db| Ok(
                    db.query_row("SELECT count(*) FROM thread", [], |r| r.get::<_, i64>(0))?
                ))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            control.shutdown().await.unwrap_err().code,
            ErrorCode::SaveFailed
        );
        owner.shutdown().await.unwrap();
    }
}

async fn inspection_work<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T> {
    static SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let permit = SLOTS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| error(ErrorCode::InvalidState, "Inspection workers closed"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| error(ErrorCode::IoFailed, "Inspection worker failed"))
}

fn open_inspection(path: &std::path::Path) -> Result<rusqlite::Connection> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sql_error)?;
    db.busy_timeout(std::time::Duration::from_millis(250))
        .map_err(sql_error)?;
    db.pragma_update(None, "query_only", true)
        .map_err(sql_error)?;
    Ok(db)
}

#[cfg(test)]
mod inspection_isolation_tests {
    use super::*;
    use crate::storage::DbWorker;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "benchmark requires a disposable database copy and explicit environment"]
    async fn benchmark_history_lane_and_wal() {
        use std::{
            path::PathBuf,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
            time::Instant,
        };
        let path =
            PathBuf::from(std::env::var_os("SHELLBOY_BENCH_DB").expect("disposable copy required"));
        let thread_id = std::env::var("SHELLBOY_BENCH_THREAD").expect("thread required");
        let old = std::env::var_os("SHELLBOY_BENCH_OLD").is_some();
        let hold_snapshot = std::env::var("SHELLBOY_BENCH_HOLD_SNAPSHOT_SECS")
            .ok()
            .map(|value| value.parse::<u64>().expect("snapshot hold must be seconds"))
            .map(std::time::Duration::from_secs)
            .unwrap_or_default();
        let writer = DbWorker::open(&path).await.unwrap();
        writer.call(|db| { db.execute_batch("CREATE TABLE IF NOT EXISTS benchmark_scratch (x INTEGER); PRAGMA wal_autocheckpoint=0;")?; Ok(()) }).await.unwrap();
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let size = || std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        let initial = size();
        let args = crate::protocol::HistoryListArgs {
            thread_id,
            limit: 40,
            excerpt_chars: 1200,
            cursor: None,
            run_id: None,
            since: Some("2026-11-03T09:30:00+09:00".into()),
            state: None,
        };
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let started = Instant::now();
        let done = Arc::new(AtomicBool::new(false));
        let reader_done = done.clone();
        let reader = if old {
            let w = writer.clone();
            tokio::spawn(async move {
                let r = w
                    .call(move |db| {
                        let tx = db.transaction()?;
                        let _ = ready_tx.send(());
                        let r = crate::history::list_nodes(&tx, &args);
                        tx.commit()?;
                        Ok(r)
                    })
                    .await
                    .unwrap();
                reader_done.store(true, Ordering::SeqCst);
                r
            })
        } else {
            let p = path.clone();
            tokio::spawn(async move {
                let r = tokio::task::spawn_blocking(move || {
                    let mut db = open_inspection(&p).unwrap();
                    let tx = db.transaction().unwrap();
                    let _: i64 = tx
                        .query_row("SELECT count(*) FROM benchmark_scratch", [], |r| r.get(0))
                        .unwrap();
                    let snapshot_started = Instant::now();
                    let _ = ready_tx.send(());
                    let r = crate::history::list_nodes(&tx, &args);
                    let remaining = hold_snapshot.saturating_sub(snapshot_started.elapsed());
                    std::thread::sleep(remaining);
                    tx.commit().unwrap();
                    r
                })
                .await
                .unwrap();
                reader_done.store(true, Ordering::SeqCst);
                r
            })
        };
        ready_rx.await.unwrap();
        let mut maximum = initial;
        let mut max_write = std::time::Duration::ZERO;
        let mut writes = 0;
        while !done.load(Ordering::SeqCst) {
            let t = Instant::now();
            writer
                .call(|db| {
                    db.execute("INSERT INTO benchmark_scratch VALUES(1)", [])?;
                    Ok(())
                })
                .await
                .unwrap();
            max_write = max_write.max(t.elapsed());
            writes += 1;
            maximum = maximum.max(size());
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let result = reader.await.unwrap();
        let elapsed = started.elapsed();
        assert!(result.is_ok(), "history failed: {:?}", result.err());
        let final_size = size();
        let checkpoint = writer
            .call(|db| {
                Ok(db.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?)
            })
            .await
            .unwrap();
        eprintln!(
            "old={old} elapsed={elapsed:?} snapshot_hold_target={hold_snapshot:?} max_write={max_write:?} writes={writes} wal_start={initial} wal_max={maximum} wal_end={final_size} checkpoint={checkpoint:?}"
        );
        writer.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn held_inspections_do_not_block_presentation() {
        let (ready_tx, ready_rx) = tokio::sync::mpsc::unbounded_channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let mut reads = Vec::new();
        for _ in 0..2 {
            let ready = ready_tx.clone();
            let release = release_rx.clone();
            reads.push(tokio::spawn(inspection_work(move || {
                ready.send(()).unwrap();
                release.lock().unwrap().recv().unwrap();
            })));
        }
        let mut ready_rx = ready_rx;
        crate::test_watchdog::watchdog("two inspections start", async {
            ready_rx.recv().await;
            ready_rx.recv().await
        })
        .await
        .unwrap();
        let projection = crate::test_watchdog::watchdog(
            "presentation during inspections",
            crate::presentation::work(|| 42),
        )
        .await;
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        for read in reads {
            read.await.unwrap().unwrap();
        }
        assert_eq!(projection.unwrap().unwrap(), 42);
    }

    #[tokio::test]
    async fn snapshot_reader_does_not_hold_writer_lane() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inspection.sqlite");
        let writer = DbWorker::open(&path).await.unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let read_path = path.clone();
        let reader = tokio::task::spawn_blocking(move || {
            let mut db = open_inspection(&read_path).unwrap();
            let tx = db.transaction().unwrap();
            let _: i64 = tx
                .query_row("SELECT count(*) FROM app_meta", [], |r| r.get(0))
                .unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            tx.commit().unwrap();
        });
        crate::test_watchdog::watchdog("inspection reader started", ready_rx)
            .await
            .unwrap()
            .unwrap();
        let start = std::time::Instant::now();
        let result = crate::test_watchdog::watchdog(
            "writer during inspection",
            writer.call(|db| {
                db.execute(
                    "INSERT INTO app_meta(key,value) VALUES('inspection-isolation','ok')",
                    [],
                )?;
                Ok(())
            }),
        )
        .await;
        let elapsed = start.elapsed();
        release_tx.send(()).unwrap();
        reader.await.unwrap();
        result.unwrap().unwrap();
        eprintln!("writer while read held: {elapsed:?}");
        writer.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod retry_tracking_diagnosis_287 {
    use super::*;

    // Real retry/recovery/readback on synthetic SQL, without provider execution.
    // No DataOwner, driver, network, provider or shell is started.
    #[test]
    fn diagnosis_287_existing_turn_and_cursor_track_four_retry_outcomes() {
        for case in [
            "success",
            "same_failure",
            "different_failure",
            "stopped",
            "no_attempt_failure",
        ] {
            let mut db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread','/','','synthetic','synthetic','before'); INSERT INTO run(id,thread_id,state,created_at)VALUES('run','thread','live','before'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','thread','run','running','before');").unwrap();
            let old_error = ApiError::new(ErrorCode::InvalidState, "synthetic HTTP400 original");
            let tx = db.transaction().unwrap();
            assert!(
                crate::control::state::recover_turn_tx(&tx, "thread", "run", "turn", 0, &old_error)
                    .unwrap()
            );
            tx.commit().unwrap();
            let conditions = vec![WaitCondition::ThreadSettled {
                thread_ids: vec!["thread".into()],
                states: vec![
                    "completed".into(),
                    "recovery_wait".into(),
                    "failed".into(),
                    "stopped".into(),
                ],
            }];
            let baseline = crate::thread_observation::observe(&db, &conditions, None, true)
                .unwrap()
                .unwrap();
            assert_eq!(baseline.changes.len(), 1);
            let tx = db.transaction().unwrap();
            let accepted = retry_run(&tx, "run", None, "accepted").unwrap().result;
            tx.commit().unwrap();
            assert_eq!(accepted["run_id"], "run");
            assert_eq!(accepted["turn_id"], "turn");
            assert_eq!(accepted["state"], "live");
            assert!(accepted.get("recovery_hint").is_none());
            assert!(accepted.get("attempt_id").is_none());
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0,
                "acceptance does not create an attempt or prove model execution"
            );
            let preparing = serde_json::to_value(turn_details(&db, "turn").unwrap()).unwrap();
            assert_eq!(preparing["state"], "preparing");
            assert_eq!(
                db.query_row("SELECT generation FROM turn WHERE id='turn'", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert!(
                crate::thread_observation::observe(&db, &conditions, Some(&baseline.cursor), false)
                    .unwrap()
                    .unwrap()
                    .changes
                    .is_empty()
            );
            let tx = db.transaction().unwrap();
            if case != "no_attempt_failure" {
                tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('retry-attempt','turn',1,0,'synthetic','synthetic',0,'inflight','{}','accepted')", []).unwrap();
                tx.execute("UPDATE turn SET state='running' WHERE id='turn'", [])
                    .unwrap();
            }
            match case {
                "success" => {
                    tx.execute_batch("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('answer','turn','assistant_response','finished','{\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"synthetic answer\"}]}]}'); UPDATE turn SET state='completed',answer_node_id='answer',ended_at='finished' WHERE id='turn'; UPDATE run SET state='completed',ended_at='finished' WHERE id='run'; UPDATE model_attempt SET state='completed',ended_at='finished' WHERE id='retry-attempt';").unwrap();
                }
                "same_failure" | "different_failure" | "no_attempt_failure" => {
                    let error = if case != "different_failure" {
                        old_error.clone()
                    } else {
                        ApiError::new(ErrorCode::InvalidState, "synthetic HTTP503 changed")
                    };
                    assert!(
                        crate::control::state::recover_turn_tx(
                            &tx, "thread", "run", "turn", 1, &error
                        )
                        .unwrap()
                    );
                    tx.execute("UPDATE model_attempt SET state='failed',error_json=?1,ended_at='finished' WHERE id='retry-attempt'", [serde_json::to_string(&error).unwrap()]).unwrap();
                }
                "stopped" => {
                    tx.execute_batch("UPDATE turn SET state='stopped',ended_at='finished' WHERE id='turn'; UPDATE run SET state='stopped',stop_reason='explicit_stop',ended_at='finished' WHERE id='run'; UPDATE model_attempt SET state='cancelled',ended_at='finished' WHERE id='retry-attempt';").unwrap();
                }
                _ => unreachable!(),
            }
            tx.commit().unwrap();
            let result = serde_json::to_value(turn_details(&db, "turn").unwrap()).unwrap();
            let expected = match case {
                "success" => "completed",
                "stopped" => "stopped",
                _ => "recovery_wait",
            };
            assert_eq!(result["state"], expected);
            assert_eq!(result["turn_id"], accepted["turn_id"]);
            if case == "success" {
                assert_eq!(result["end_message_node_id"], "answer");
            }
            if case.ends_with("failure") {
                let reason = &result["context"]["recovery_reason"];
                assert_eq!(
                    reason["message"],
                    if case != "different_failure" {
                        "synthetic HTTP400 original"
                    } else {
                        "synthetic HTTP503 changed"
                    }
                );
            }
            if case == "no_attempt_failure" {
                assert_eq!(
                    db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
            }
            // Finish an unrelated independent Run in the same Thread. The
            // Thread-wide cursor is only a wake hint; exact target IDs decide.
            db.execute_batch("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('other-run','thread','completed','later','finished');").unwrap();
            let observed =
                crate::thread_observation::observe(&db, &conditions, Some(&baseline.cursor), false)
                    .unwrap()
                    .unwrap();
            assert_eq!(
                observed.changes.len(),
                2,
                "both target and unrelated changes remain visible"
            );
            let target: Vec<_> = observed
                .changes
                .iter()
                .filter(|change| {
                    change["run_id"] == accepted["run_id"]
                        && (change.get("turn_id").is_none()
                            || change["turn_id"] == accepted["turn_id"])
                })
                .collect();
            assert_eq!(
                target.len(),
                1,
                "filter the acknowledgement IDs, never Thread latest"
            );
            assert_eq!(target[0]["state"], expected);
            assert!(
                crate::thread_observation::observe(&db, &conditions, Some(&observed.cursor), false)
                    .unwrap()
                    .unwrap()
                    .changes
                    .is_empty(),
                "cursor consumption does not repeat a transition"
            );
            db.execute("DELETE FROM event WHERE id='recovery-turn-0'", [])
                .unwrap();
            assert_eq!(
                crate::thread_observation::observe(&db, &conditions, Some(&baseline.cursor), false)
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::CursorExpired,
                "lost baseline cannot silently identify an earlier retry"
            );
            eprintln!(
                "{case}: ack run=run turn=turn attempts=0; saved outcome={expected}; exact target change=1, unrelated=1, consumed=0, expired baseline rejected"
            );
        }
    }
    #[tokio::test]
    async fn retry_queue_unit_and_saved_receipt_are_not_execution_outcomes() {
        let root = tempfile::tempdir().unwrap();
        let database = DbWorker::open(root.path().join("synthetic.db"))
            .await
            .unwrap();
        database.call(|db| {
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','synthetic','synthetic','before'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','before'); INSERT INTO turn(id,thread_id,run_id,state,created_at,recovery_reason_json)VALUES('turn','t','r','recovery_wait','before','{}');")?;
            for n in 0..129 {
                let id=format!("held-{n:03}");
                db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'turn','user','before','{}')",[&id])?;
                db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,path_admission,created_at)VALUES(?1,?1,'null','t',?1,'r','turn','held','unadmitted','before')",[&id])?;
                db.execute("UPDATE receipt SET hold_reason='recovery_wait' WHERE id=?1",[&id])?;
            }
            Ok(())
        }).await.unwrap();
        let config=Config::parse("default_connection='synthetic'\ndefault_model='synthetic'\n[connections.synthetic]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
        let store = Store::new(
            database.clone(),
            Arc::new(config),
            root.path().into(),
            "fixture".into(),
        );
        let command = Command::RunRetry(RunTarget { run_id: "r".into() });
        let request = Request::new("fixture:retry", command.clone());
        let accepted = store.execute(request.clone(), None).await.unwrap().result;
        let hint: Value = serde_json::from_str(
            accepted["recovery_hint"]
                .as_str()
                .expect("existing serialized recovery hint"),
        )
        .unwrap();
        assert_eq!(hint["remaining_held"], 1);
        let replay = store.execute(request.clone(), None).await.unwrap().result;
        assert_eq!(replay, accepted);
        database
            .call(|db| {
                assert_eq!(
                    serde_json::to_value(turn_details(db, "turn").unwrap()).unwrap()["state"],
                    "recovery_wait"
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM thread_path", [], |r| r
                        .get::<_, i64>(0))?,
                    128,
                    "same-ID receipt replay is not another queue unit"
                );
                Ok(())
            })
            .await
            .unwrap();
        let next = store
            .execute(Request::new("fixture:next-unit", command), None)
            .await
            .unwrap()
            .result;
        assert!(next.get("recovery_hint").is_none());
        assert_eq!(
            store.execute(request, None).await.unwrap().result,
            accepted,
            "replayed acceptance does not become the current outcome"
        );
        database
            .call(|db| {
                assert_eq!(
                    serde_json::to_value(turn_details(db, "turn").unwrap()).unwrap()["state"],
                    "preparing"
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM thread_path", [], |r| r
                        .get::<_, i64>(0))?,
                    129
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        eprintln!(
            "queue: real Store first=128 remaining1, same-ID replay unchanged, new-ID next=1, attempts=0"
        );
        drop(store);
        database.shutdown().await.unwrap();
    }
}

fn budget_history<T: serde::Serialize>(
    operation: &str,
    result: T,
    slots: crate::history::message_budget::Slots,
) -> Result<MutationResult> {
    let result = serde_json::to_value(result).map_err(|_| {
        error(
            ErrorCode::InvalidState,
            "History result serialization failed",
        )
    })?;
    let mut response = crate::protocol::Response::success(operation, result);
    crate::history::message_budget::fit(&mut response, slots)
        .map_err(|_| error(ErrorCode::InvalidState, "History provenance budget failed"))?;
    MutationResult::value(response.result.expect("success response owns its result"))
}
