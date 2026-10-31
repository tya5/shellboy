//! Durable Control operations. Runtime effects are returned only after commit.
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
    pub now: bool,
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
    let mut intent = json!({"internal_operation":"draft_send_raw","args":args,"thread_id":thread,"text":content.text,"files":files,"now":options.now,"submission":options.submission});
    content.fingerprint_positions(&mut intent);
    Ok(hash(serde_json::to_vec(&intent).map_err(|_| {
        error(ErrorCode::InvalidArgument, "Invalid draft request identity")
    })?))
}

#[derive(Debug, Clone, Default)]
pub struct MutationResult {
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
    fn value(result: impl Serialize) -> Result<Self> {
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
}

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
        let clients = Arc::new(crate::model::http::HttpClients::new(&config));
        Self {
            index: crate::index::IndexService::with_clients(
                database.clone(),
                config.clone(),
                instance_id.clone(),
                clients.clone(),
            ),
            clients,
            instructions: crate::instructions::InstructionLoader::new(&config),
            database,
            config,
            startup_cwd,
            instance_id,
            usage_diagnostics: None,
            usage_context: crate::usage_totals::ReadContext::default(),
        }
    }

    pub async fn collect_receipts(&self) -> Result<u64> {
        let instance = self.instance_id.clone();
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                let result = super::receipt::under_pressure(&tx, &instance)
                    .and_then(|pressure| super::receipt::collect(&tx, &instance, pressure));
                if result.is_ok() {
                    tx.commit()?;
                }
                Ok(result)
            })
            .await
            .map_err(storage_error)?
    }

    /// `caller` has already been authenticated against the server's origin map.
    /// A client-supplied Request.origin is never independently trusted here.
    pub async fn execute(
        &self,
        request: Request,
        caller: Option<Origin>,
    ) -> Result<MutationResult> {
        request.validate()?;
        if let Command::Status(mut args) = request.command.clone() {
            if args.thread_id.is_none() && args.run_id.is_none() {
                args.thread_id = caller.as_ref().map(|origin| origin.thread_id.clone());
            }
            let instance = self.instance_id.clone();
            return self
                .database
                .call(move |db| {
                    let tx = db.transaction()?;
                    let result = crate::status::snapshot(&tx, &args, &instance);
                    tx.commit()?;
                    Ok(result)
                })
                .await
                .map_err(storage_error)?
                .and_then(MutationResult::value);
        }
        if let Command::AttachmentList(args) = &request.command {
            let thread = target(args.thread_id.as_deref(), caller.as_ref())?;
            let path = self.database.path().to_path_buf();
            let mut items = crate::presentation::work(move || {
                let mut db = rusqlite::Connection::open_with_flags(
                    path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )
                .map_err(sql_error)?;
                db.busy_timeout(std::time::Duration::from_millis(250))
                    .map_err(sql_error)?;
                db.pragma_update(None, "query_only", true)
                    .map_err(sql_error)?;
                let tx = db.transaction().map_err(sql_error)?;
                let items = crate::attachments::list_tx(&tx, &thread)?;
                tx.commit().map_err(sql_error)?;
                drop(db);
                Ok::<_, ApiError>(items)
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
        if matches!(
            request.command,
            Command::UsageGet(_)
                | Command::ThreadGet(_)
                | Command::RunGet(_)
                | Command::TurnGet(_)
                | Command::CompactList(_)
                | Command::ShellList(_)
        ) {
            let mut store = self.clone();
            store.usage_context = crate::usage_totals::read_context();
            let path = self.database.path().to_path_buf();
            return crate::presentation::work(move || {
                let mut db = rusqlite::Connection::open_with_flags(
                    path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )
                .map_err(sql_error)?;
                db.busy_timeout(std::time::Duration::from_millis(250))
                    .map_err(sql_error)?;
                db.pragma_update(None, "query_only", true)
                    .map_err(sql_error)?;
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
            .await?;
        }
        let plan = self.prepare_agents(&request, &caller).await?;
        let mut store = self.clone();
        store.usage_context = crate::usage_totals::read_context();
        self.database
            .call(move |connection| {
                if !request.command.requires_receipt() {
                    // Use the canonical query projections without reserving,
                    // replaying, or collecting immutable mutation receipts.
                    let tx = connection.transaction()?;
                    let result = store.dispatch(
                        &tx,
                        &request.command,
                        caller.as_ref(),
                        &request.operation_id,
                        &chrono::Utc::now().to_rfc3339(),
                    );
                    tx.commit()?;
                    return Ok(result);
                }
                Ok(store.execute_transaction(connection, &request, caller.as_ref(), plan.as_ref()))
            })
            .await
            .map_err(storage_error)?
    }

    pub async fn execute_with_plan(
        &self,
        request: Request,
        caller: Option<Origin>,
        plan: crate::attachments::AddPlan,
    ) -> Result<MutationResult> {
        request.validate()?;
        let store = self.clone();
        self.database
            .call(move |db| {
                let fingerprint = hash(
                    serde_json::to_vec(&json!({"command":request.command,"caller":caller}))
                        .unwrap(),
                );
                Ok(store.transact(
                    db,
                    &request.operation_id,
                    &fingerprint,
                    caller.as_ref(),
                    Admission::Normal,
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
                        if live && !held { result.quiet_wake_threads.push(target_id); }
                        Ok(result)
                    },
                ))
            })
            .await
            .map_err(storage_error)?
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
        let mut intent = json!({"internal_operation":"draft_send","args":args,"thread_id":thread_id,"text":content.text,"files":paths,"now":options.now,"submission":options.submission});
        content.fingerprint_positions(&mut intent);
        let fingerprint = options
            .request_fingerprint
            .clone()
            .unwrap_or_else(|| hash(serde_json::to_vec(&intent).unwrap()));
        let store = self.clone();
        self.database
            .call(move |db| {
                Ok(store.transact(
                    db,
                    &operation_id,
                    &fingerprint,
                    None,
                    Admission::Normal,
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
                                sender_label: None,
                                thread_id: Some(target),
                                text: content.text.clone(),
                                now: options.now,
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
                                files,
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
            .map_err(storage_error)?
    }

    /// Read a committed receipt before mutable filesystem/target preflight.
    pub async fn replay(
        &self,
        request: &Request,
        caller: &Option<Origin>,
    ) -> Result<Option<MutationResult>> {
        let fingerprint = hash(
            serde_json::to_vec(&json!({"command":request.command,"caller":caller}))
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
            serde_json::to_vec(&json!({"command":request.command,"caller":caller}))
                .map_err(|_| error(ErrorCode::InvalidArgument, "invalid request encoding"))?,
        );
        let preflight = super::receipt::runtime_preflight(&request).err();
        let admission = Admission::for_command(&request.command);
        self.reserve_hashed(
            request.operation_id,
            caller,
            fingerprint,
            preflight,
            admission,
        )
        .await
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
        self.database.call(move|db| {
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
                if error.code==ErrorCode::StorageLimit {tx.commit()?;}
                return Ok(Err(error));
            }
            tx.commit()?;
            Ok(Ok(RuntimeReservation::New))
        }).await.map_err(storage_error)?
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
                let effects = stop_run(&tx, &run, None, &chrono::Utc::now().to_rfc3339());
                if effects.is_ok() {
                    tx.commit()?;
                }
                Ok(effects)
            })
            .await
            .map_err(storage_error)?
    }

    pub async fn stop_for_edit(
        &self,
        thread: String,
        confirmed: Option<String>,
    ) -> Result<MutationResult> {
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                let live: Option<String> = tx
                    .query_row(
                        "SELECT id FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",
                        [&thread],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(run) = live {
                    if confirmed.as_deref() != Some(&run) {
                        return Ok(Err(error(
                            ErrorCode::InvalidState,
                            "Confirm stopping the current Run before editing this request",
                        )
                        .details(json!({"confirmation_required":true,"run_id":run}))));
                    }
                    let effects = stop_run(&tx, &run, None, &chrono::Utc::now().to_rfc3339());
                    if effects.is_ok() {
                        tx.commit()?;
                    }
                    Ok(effects)
                } else {
                    Ok(Ok(MutationResult::default()))
                }
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
                if let Some(run)=live{return Err(error(ErrorCode::InvalidState,"A new live Run appeared; confirmation is required again").details(json!({"confirmation_required":true,"run_id":run})));}
                let user:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM node n JOIN thread_path p ON p.node_id=n.id WHERE p.thread_id=?1 AND n.id=?2 AND n.deleted_reason IS NULL AND json_extract(n.payload_json,'$.role')='user')",params![thread,node],|r|r.get(0)).map_err(sql_error)?;
                if !user{return Err(error(ErrorCode::InvalidArgument,"Edit target must be an original user request on this Thread's selected path"));}
                let fork=uuid::Uuid::new_v4().to_string();let now=chrono::Utc::now().to_rfc3339();
                history::fork_thread(&tx,&ThreadForkArgs{ agents: None,thread_id:thread.clone(),node_id:node.clone(),cwd:args.cwd.clone()},&fork,&now)?;
                let mut path:Vec<String>=tx.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal").map_err(sql_error)?.query_map([&fork],|r|r.get(0)).map_err(sql_error)?.collect::<rusqlite::Result<_>>().map_err(sql_error)?;
                if path.pop().as_deref()!=Some(&node){return Err(error(ErrorCode::InvalidState,"Edit fork boundary changed"));}
                tx.execute("DELETE FROM attachment_selection WHERE thread_id=?1 AND attachment_id IN (SELECT id FROM attachment_ref WHERE source_node_id=?2)",params![fork,node]).map_err(sql_error)?;
                history::select_path(&tx,&fork,1,&path,&now).map_err(storage_error)?;
                if args.connection.is_some()||args.model.is_some()||args.effort.is_some(){store.set_thread(&tx,&ThreadSetArgs{ agents: None,thread_id:Some(fork.clone()),connection:args.connection,model:args.model,effort:args.effort,cwd:None,title:None},None,&now)?;}
                let revision:i64=history::selection_revision(&tx, &fork, history::ThreadScope::Any).map_err(sql_error)?;
                let mut result=store.send_content(&tx,&MessageSendArgs { sender_label: None,thread_id:Some(fork),text:content.text.clone(),now:options.now},None,&operation_id,&now,SendContent { elements: content.new_source_elements(files.len())?, sender: MessageSender::recorded(None, None, true) })?;
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
    ) -> Result<MutationResult> {
        let fingerprint = hash(
            serde_json::to_vec(&json!({"command":request.command,"caller":caller}))
                .map_err(|_| error(ErrorCode::InvalidArgument, "invalid request encoding"))?,
        );
        self.transact(
            connection,
            &request.operation_id,
            &fingerprint,
            caller,
            Admission::for_command(&request.command),
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
        self.database
            .call(move |connection| {
                Ok(store.transact(
                    connection,
                    &operation_id,
                    &fingerprint,
                    None,
                    Admission::Normal,
                    |tx, now| {
                        let thread_id = store.create_thread(tx, &args, now)?;
                        store.send_content(
                            tx,
                            &MessageSendArgs {
                                sender_label: None,
                                thread_id: Some(thread_id),
                                text: text.clone(),
                                now: false,
                            },
                            None,
                            &operation_id,
                            now,
                            SendContent {
                                elements: vec![json!({"type":"input_text","text":text})],
                                sender: MessageSender::recorded(None, None, true),
                            },
                        )
                    },
                ))
            })
            .await
            .map_err(storage_error)?
    }

    fn transact(
        &self,
        connection: &mut Connection,
        operation_id: &str,
        fingerprint: &str,
        caller: Option<&Origin>,
        admission: Admission,
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
            }
            return Err(error);
        }
        tx.execute_batch("SAVEPOINT operation_mutation")
            .map_err(sql_error)?;
        let mut outcome = operation(&tx, &now);
        let projected = outcome
            .as_ref()
            .map(|r| r.result.clone())
            .map_err(Clone::clone);
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
            Ok(result) => SavedOutcome {
                result: Some(result.result.clone()),
                error: None,
            },
            Err(error) => SavedOutcome {
                result: None,
                error: Some(error.clone()),
            },
        };
        tx.execute("UPDATE operation_receipt SET state=?2,result_ref_json=?3,reserved_bytes=0 WHERE operation_id=?1",params![operation_id,if outcome.is_ok(){"completed"}else{"failed"},serde_json::to_string(&saved).unwrap()]).map_err(sql_error)?;
        tx.commit().map_err(sql_error)?;
        outcome
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
                MutationResult::value(thread_details(tx, &id)?)
            }
            Command::ThreadFork(args) => {
                let id = uuid::Uuid::new_v4().to_string();
                let mut defaults = args.clone();
                defaults.agents = None;
                history::fork_thread(tx, &defaults, &id, now)?;
                if let Some(path) = &args.agents {
                    self.set_agents_tx(tx, &id, path, caller, now)?;
                }
                MutationResult::value(thread_details(tx, &id)?)
            }
            Command::ThreadGet(args) => {
                let id = target(args.thread_id.as_deref(), caller)?;
                let mut details = thread_details(tx, &id)?;
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
            Command::ThreadSet(args) => self.set_thread(tx, args, caller, now),
            Command::ThreadDelete(args) => delete_thread(tx, args, now),
            Command::ThreadDeletePreview(args) => {
                MutationResult::value(crate::deletion::preview(tx, args)?)
            }
            Command::MessageSend(args) => self.send(tx, args, caller, operation_id, now),
            Command::MessageGet(args) => {
                MutationResult::value(message_details(tx, &args.message_id)?)
            }
            Command::MessageCancel(args) => {
                let (cancelled, changed) =
                    super::pending_messages::cancel_tx(tx, args, caller, now)
                        .map_err(storage_error)?;
                let mut result = MutationResult::value(cancelled)?;
                result.changed_threads = changed;
                Ok(result)
            }
            Command::OperationGet(args) => MutationResult::value(operation_details(
                tx,
                &args.operation_id,
                &self.instance_id,
            )?),
            Command::RevisitGet(args) => {
                MutationResult::value(super::revisit::get(tx, args, caller, &self.config)?)
            }
            Command::RevisitSet(args) => {
                let (thread, result) =
                    super::revisit::set(tx, args, caller, operation_id, now, &self.config)?;
                Ok(MutationResult {
                    result,
                    changed_threads: vec![thread],
                    ..Default::default()
                })
            }
            Command::RevisitCancel(args) => {
                let (thread, result) = super::revisit::cancel(tx, args, caller, now, &self.config)?;
                Ok(MutationResult {
                    result,
                    changed_threads: vec![thread],
                    ..Default::default()
                })
            }
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
            Command::RunStop(args) => stop_run(tx, &args.run_id, caller, now),
            Command::RunRetry(args) => retry_run(tx, &args.run_id, caller, now),
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
                crate::attachments::remove_tx(
                    tx,
                    &target(args.thread_id.as_deref(), caller)?,
                    &args.attachment_id,
                )?;
                MutationResult::value(json!({"attachment_id":args.attachment_id,"removed":true}))
            }
            Command::HistoryList(args) => MutationResult::value(history::list_nodes(tx, args)?),
            Command::HistoryGet(args) => MutationResult::value(history::get_node(tx, args)?),
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
                            |r| Ok((r.get(0)?, r.get(1)?)),
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
        let inherited = if let Some(parent) = &args.parent_id {
            Some(tx.query_row("SELECT cwd,agents_path,profile_id,model_id,effort FROM thread WHERE id=?1 AND deleted_at IS NULL",[parent],|row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?))).optional().map_err(sql_error)?.ok_or_else(|| error(ErrorCode::ThreadNotFound,"parent Thread not found"))?)
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
            .or_else(|| self.config.default_connection.clone())
            .ok_or_else(|| {
                crate::config_setup::enrich(
                    &self.config,
                    None,
                    args.model
                        .as_deref()
                        .or(self.config.default_model.as_deref()),
                    error(
                        ErrorCode::ConfigInvalid,
                        "set a default connection or choose --connection",
                    ),
                )
            })?;
        let connection = self.config.connections.get(&profile).ok_or_else(|| {
            crate::config_setup::enrich(
                &self.config,
                Some(&profile),
                args.model
                    .as_deref()
                    .or(self.config.default_model.as_deref()),
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
            .or_else(|| self.config.default_model.clone())
            .ok_or_else(|| {
                crate::config_setup::enrich(
                    &self.config,
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
        tx.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,effort,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![id,args.parent_id,cwd.to_string_lossy(),agents,profile,model,effort,now]).map_err(sql_error)?;
        bump_roots(tx)?;
        Ok(id)
    }

    fn set_thread(
        &self,
        tx: &Transaction<'_>,
        args: &ThreadSetArgs,
        caller: Option<&Origin>,
        now: &str,
    ) -> Result<MutationResult> {
        let id = target(args.thread_id.as_deref(), caller)?;
        let current = thread_details(tx, &id)?;
        let title = args.title.as_deref().map(str::trim);
        if args.connection.is_none()
            && args.model.is_none()
            && args.cwd.is_none()
            && args.effort.is_none()
            && args.agents.is_none()
        {
            tx.execute("UPDATE thread SET title=COALESCE(?2,title),revision=revision+1,updated_at=?3 WHERE id=?1",params![id,title,now]).map_err(sql_error)?;
            let mut result = MutationResult::value(thread_details(tx, &id)?)?;
            result.changed_threads.push(id);
            return Ok(result);
        }
        let profile = args.connection.as_deref().unwrap_or(&current.connection);
        if !self.config.connections.contains_key(profile) {
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
            super::context::record(tx,&id,"thread_settings",actor(caller),json!({"before":{"cwd":current.cwd,"connection":current.connection,"model":current.model,"effort":current.effort},"after":{"cwd":after.cwd,"connection":after.connection,"model":after.model,"effort":after.effort}}),now).map_err(sql_error)?;
        }
        let mut result = MutationResult::value(after)?;
        result.changed_threads.push(id);
        Ok(result)
    }

    fn send(
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
                sender: MessageSender::recorded(caller, args.sender_label.clone(), false),
            },
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
        let id = target(args.thread_id.as_deref(), caller)?;
        let MessageAssignment {
            run: assigned_run,
            turn: assigned_turn,
            assignment,
            status,
            reason,
            wake,
            owner_changed,
        } = assign_message(tx, &id, caller, now)?;
        let message_id = uuid::Uuid::new_v4().to_string();
        let payload = json!({"role":"user","content":content.elements});
        let body = payload.to_string();
        tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES(?1,?2,'user',?3,?4,?5,?6)",params![message_id,assigned_turn,now,body,body.len() as i64,hash(body.as_bytes())]).map_err(sql_error)?;
        self.index
            .authorize_new_tx(tx, std::slice::from_ref(&message_id))?;
        let receipt_id = uuid::Uuid::new_v4().to_string();
        let state = match status {
            DeliveryStatus::Accepted => "accepted",
            DeliveryStatus::Held => "held",
            _ => unreachable!(),
        };
        let accepted = MessageAccepted {
            message_id: message_id.clone(),
            thread_id: id.clone(),
            delivery_status: status,
            assignment,
            now: args.now,
            run_id: assigned_run,
            turn_id: assigned_turn,
        };
        let saved_acceptance = serde_json::to_value(&accepted)
            .map_err(|_| error(ErrorCode::InvalidState, "Cannot encode message acceptance"))?;
        tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,updated_at,hold_reason,result_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11)",params![receipt_id,operation_id,serde_json::to_string(&content.sender).unwrap(),id,message_id,accepted.run_id,accepted.turn_id,state,now,reason,saved_acceptance.to_string()]).map_err(sql_error)?;
        tx.execute(
            "UPDATE thread SET revision=revision+1,updated_at=?2 WHERE id=?1",
            params![id, now],
        )
        .map_err(sql_error)?;
        bump_roots(tx)?;
        let mut result = MutationResult::value(saved_acceptance)?;
        if let Some(owner) = owner_changed {
            result.changed_threads.push(owner);
        }
        if wake {
            if args.now {
                result.wake_threads.push(id);
            } else {
                result.quiet_wake_threads.push(id);
            }
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
    owner_changed: Option<String>,
}

fn assign_message(
    tx: &Transaction<'_>,
    id: &str,
    caller: Option<&Origin>,
    now: &str,
) -> Result<MessageAssignment> {
    let parent: Option<String> = tx
        .query_row(
            "SELECT parent_thread_id FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| error(ErrorCode::ThreadNotFound, "target Thread not found"))?;
    let run:Option<(String,String,Option<String>)>=tx.query_row("SELECT id,state,owner_run_id FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(sql_error)?;
    let mut assigned_run = None;
    let mut assigned_turn = None;
    let mut assignment = Assignment::Pending;
    let mut status = DeliveryStatus::Accepted;
    let mut reason = None;
    let mut wake = false;
    let mut owner_changed = None;
    if let Some((run_id, state, owner)) = run {
        if let Some(caller) = caller
            && parent.as_deref() == Some(&caller.thread_id)
            && owner.as_deref().is_some_and(|owner| owner != caller.run_id)
        {
            return Err(error(
                ErrorCode::InvalidState,
                "child Run is owned by another live parent Run",
            ));
        }
        if state == "stopping" {
            status = DeliveryStatus::Held;
            reason = Some("stopping");
        } else {
            assigned_run = Some(run_id.clone());
            let turn:Option<(String,String,i64)>=tx.query_row("SELECT id,state,generation FROM turn WHERE run_id=?1 AND state IN ('preparing','running','waiting','recovery_wait')",[&run_id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(sql_error)?;
            if let Some((turn_id, state, generation)) = turn {
                assignment = Assignment::ExistingTurn;
                if state == "recovery_wait" && caller.is_some() {
                    status = DeliveryStatus::Held;
                    reason = Some("recovery_wait");
                } else {
                    if state == "recovery_wait" {
                        if !super::state::resume_recovery_turn_tx(
                            tx, id, &run_id, &turn_id, generation, now,
                        )
                        .map_err(sql_error)?
                        {
                            return Err(error(
                                ErrorCode::OperationConflict,
                                "Turn changed before explicit recovery resume",
                            ));
                        }
                        super::context::record(
                            tx,
                            id,
                            "recovery_resumed",
                            actor(caller),
                            json!({"run_id":run_id,"turn_id":turn_id}),
                            now,
                        )
                        .map_err(sql_error)?;
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
        let owner = if let Some(caller) =
            caller.filter(|caller| parent.as_deref() == Some(caller.thread_id.as_str()))
        {
            let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND thread_id=?2 AND state='live')",params![caller.run_id,caller.thread_id],|row| row.get(0)).map_err(sql_error)?;
            if !live {
                return Err(error(
                    ErrorCode::InvalidState,
                    "owner Run is stopping or ended",
                ));
            }
            Some(caller.run_id.clone())
        } else {
            None
        };
        owner_changed = caller
            .filter(|_| owner.is_some())
            .map(|origin| origin.thread_id.clone());
        let run_id = uuid::Uuid::new_v4().to_string();
        tx.execute("INSERT INTO run(id,thread_id,owner_run_id,state,created_at) VALUES(?1,?2,?3,'live',?4)",params![run_id,id,owner,now]).map_err(sql_error)?;
        super::context::record(
            tx,
            id,
            "run_started",
            actor(caller),
            json!({"run_id":run_id}),
            now,
        )
        .map_err(sql_error)?;
        let turn_id = super::state::create_turn_tx(tx, id, &run_id, now, now).map_err(sql_error)?;
        // A human's new instruction, or an owning parent's explicit terminal
        // child restart, is the resume boundary for unassigned saved input.
        // Previously assigned receipts keep their original Run/Turn forever.
        if caller.is_none() || owner.is_some() {
            super::message_delivery::recover_on_resume_tx(
                tx,
                id,
                &run_id,
                &turn_id,
                actor(caller),
                now,
            )
            .map_err(storage_error)?;
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
        owner_changed,
    })
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
    let mut details=connection.query_row("SELECT title,cwd,profile_id,model_id,effort,parent_thread_id,agents_path FROM thread WHERE id=?1 AND deleted_at IS NULL",[id],|row|Ok(ThreadDetails { last_compaction:None,usage_totals:None, agents_path: row.get(6)?,context_usage:None,thread_id:id.into(),title:row.get(0)?,cwd:row.get(1)?,connection:row.get(2)?,model:row.get(3)?,effort:row.get(4)?,parent_id:row.get(5)?,run_id:None,turn_id:None,children_count:0,shells_count:0,recovery_reason:None,context:None})).optional().map_err(sql_error)?.ok_or_else(||error(ErrorCode::ThreadNotFound,"Thread not found"))?;
    details.context = Some(crate::context::for_thread(connection, id)?);
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
            details.turn_id = Some(id);
            details.recovery_reason = reason;
        }
        details.children_count = count(
            connection,
            "SELECT count(*) FROM run WHERE owner_run_id=?1 AND state IN ('live','stopping')",
            run,
        )?;
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
    Ok(if user_exists {
        "名称未設定"
    } else {
        "未送信"
    }
    .into())
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

const THREAD_LIST_SQL: &str = "SELECT t.id,t.title,t.cwd,t.parent_thread_id,t.updated_at,
    COALESCE(u.state,latest.state), latest.stop_reason,latest.id,latest.state,u.state,
    EXISTS(SELECT 1 FROM shell_execution WHERE owner_run_id=latest.id AND state IN ('starting','running','stopping'))
      OR EXISTS(SELECT 1 FROM run WHERE owner_run_id=latest.id AND state IN ('live','stopping')),
    a.state,CASE WHEN a.state='failed' AND a.generation=u.generation AND json_valid(a.error_json)
      THEN json_extract(a.error_json,'$.automatic_retry') END,
    CASE WHEN latest.state='live' AND u.state='running' AND COALESCE(a.state,'')<>'inflight'
      THEN EXISTS(SELECT 1 FROM tool_group WHERE turn_id=u.id AND state='running') ELSE 0 END
FROM thread t LEFT JOIN run latest ON latest.id=(
    SELECT r.id FROM run r WHERE r.thread_id=t.id ORDER BY r.created_at DESC,r.id DESC LIMIT 1)
LEFT JOIN turn u ON u.id=(SELECT id FROM turn WHERE run_id=latest.id ORDER BY created_at DESC,id DESC LIMIT 1)
LEFT JOIN model_attempt a ON a.rowid=(SELECT rowid FROM model_attempt WHERE turn_id=u.id
    AND latest.state='live' AND u.state='running' ORDER BY rowid DESC LIMIT 1)
WHERE t.deleted_at IS NULL AND t.rowid<=?1
    AND (?2='' OR t.updated_at<?2 OR (t.updated_at=?2 AND t.id<?3))
ORDER BY t.updated_at DESC,t.id DESC LIMIT ?4";

fn thread_list(connection: &Connection, args: &ThreadListArgs) -> Result<Page<ThreadSummary>> {
    let filter = args.query.as_deref().unwrap_or("");
    let needle = title_text(filter).to_lowercase();
    let watermark: i64 = connection
        .query_row("SELECT COALESCE(MAX(rowid),0) FROM thread", [], |row| {
            row.get(0)
        })
        .map_err(sql_error)?;
    let (key, watermark) = if let Some(token) = &args.cursor {
        let cursor = PageCursor::decode(token, "thread.list", filter, 0, None)?;
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
    let mut statement = connection.prepare(THREAD_LIST_SQL).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            watermark,
            last_time,
            last_id,
            if needle.is_empty() {
                i64::from(args.limit) + 1
            } else {
                -1
            }
        ])
        .map_err(sql_error)?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let id: String = row.get(0).map_err(sql_error)?;
        let stored: String = row.get(1).map_err(sql_error)?;
        let title = thread_display_title(connection, &id, &stored)?;
        let cwd: String = row.get(2).map_err(sql_error)?;
        if !needle.is_empty()
            && !title_text(&format!("{id} {title} {cwd}"))
                .to_lowercase()
                .contains(&needle)
        {
            continue;
        }
        let excerpt = thread_excerpt(connection, &id)?;
        let run: Option<String> = row.get(8).map_err(sql_error)?;
        let turn: Option<String> = row.get(9).map_err(sql_error)?;
        let attempt: Option<String> = row.get(11).map_err(sql_error)?;
        let diagnostic: Option<String> = row.get(12).map_err(sql_error)?;
        let retrying = diagnostic
            .as_deref()
            .and_then(|s| {
                serde_json::from_str::<crate::ui::activity::AutomaticRetrySnapshot>(s).ok()
            })
            .is_some_and(|r| r.number > 0 && r.number <= r.limit);
        items.push(ThreadSummary {
            activity: crate::activity::classify(
                run.as_deref(),
                turn.as_deref(),
                row.get(10).map_err(sql_error)?,
                attempt.as_deref() == Some("inflight"),
                retrying,
                row.get(13).map_err(sql_error)?,
            ),
            thread_id: id,
            title,
            cwd,
            parent_id: row.get(3).map_err(sql_error)?,
            latest_run_stop_reason: row.get(6).map_err(sql_error)?,
            revisit: row
                .get::<_, Option<String>>(7)
                .map_err(sql_error)?
                .map(|run| super::revisit::summary(connection, &run))
                .transpose()?
                .flatten(),
            updated_at: row.get(4).map_err(sql_error)?,
            state: row
                .get::<_, Option<String>>(5)
                .map_err(sql_error)?
                .unwrap_or_else(|| "idle".into()),
            excerpt,
        });
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
                scope: "thread.list".into(),
                filter: filter.into(),
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
    Ok(Page { items, next_cursor })
}

pub fn message_details(connection: &Connection, id: &str) -> Result<MessageDetails> {
    let row=connection.query_row("SELECT r.target_thread_id,r.state,r.run_id,r.turn_id,r.result_json,t.answer_node_id,r.sender_json FROM receipt r LEFT JOIN turn t ON t.id=r.turn_id WHERE message_node_id=?1",[id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,String>(6)?))).optional().map_err(sql_error)?.ok_or_else(||error(ErrorCode::TargetUnavailable,"message not found"))?;
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
        sender: MessageSender::from_saved(Some(&row.6)),
        context: crate::context::for_message(connection, id)?,
        message_id: id.into(),
        thread_id: row.0,
        delivery_status: status,
        assignment,
        run_id: row.2,
        turn_id: row.3,
        answer_node_id: row.5,
    })
}

fn saved_usage_for_detail(
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
        .query_row(
            "SELECT thread_id,state,owner_run_id FROM run WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| error(ErrorCode::TargetUnavailable, "Run not found"))?;
    let state = serde_json::from_value(json!(row.1))
        .map_err(|_| error(ErrorCode::InvalidState, "unknown saved Run state"))?;
    let turn:Option<(String,Option<String>,Option<String>)>=connection.query_row("SELECT id,answer_node_id,recovery_reason_json FROM turn WHERE run_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(sql_error)?;
    let (turn_id, answer_node_id, recovery_hint) = turn
        .map_or((None, None, None), |(id, answer, reason)| {
            (Some(id), answer, reason)
        });
    Ok(RunDetails {
        usage_totals: None,
        revisit: super::revisit::summary(connection, id)?,
        context: crate::context::for_run(connection, id)?,
        run_id: id.into(),
        thread_id: row.0,
        state,
        owner_run_id: row.2,
        turn_id,
        answer_node_id,
        recovery_hint,
        owned_run_ids: ids(
            connection,
            "SELECT id FROM run WHERE owner_run_id=?1 AND state IN ('live','stopping') ORDER BY created_at,id",
            id,
        )?,
        owned_execution_ids: ids(
            connection,
            "SELECT id FROM shell_execution WHERE owner_run_id=?1 AND state IN ('starting','running','stopping') ORDER BY started_at,id",
            id,
        )?,
    })
}

pub fn turn_details(connection: &Connection, id: &str) -> Result<TurnDetails> {
    connection.query_row("SELECT run_id,thread_id,state,answer_node_id,recovery_reason_json FROM turn WHERE id=?1",[id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,Option<String>>(4)?))).optional().map_err(sql_error)?.ok_or_else(||error(ErrorCode::TargetUnavailable,"Turn not found")).and_then(|row|Ok(TurnDetails{usage_totals:None,context:crate::context::for_turn(connection,id)?,turn_id:id.into(),run_id:row.0,thread_id:row.1,state:serde_json::from_value(json!(row.2)).map_err(|_|error(ErrorCode::InvalidState,"unknown saved Turn state"))?,answer_node_id:row.3,recovery_hint:row.4}))
}

pub(super) fn authorize_owned_run(
    db: &Connection,
    run: &str,
    caller: Option<&Origin>,
) -> Result<()> {
    let Some(caller) = caller else {
        return Ok(());
    };
    let allowed: bool = db.query_row("WITH RECURSIVE owned(id) AS (SELECT id FROM run WHERE id=?1 AND thread_id=?2 AND state IN ('live','stopping') UNION ALL SELECT r.id FROM run r JOIN owned o ON r.owner_run_id=o.id) SELECT EXISTS(SELECT 1 FROM owned WHERE id=?3)",params![caller.run_id,caller.thread_id,run],|row|row.get(0)).map_err(sql_error)?;
    if !allowed {
        return Err(error(
            ErrorCode::InvalidState,
            "Only a human or an owning Run may change this execution",
        ));
    }
    Ok(())
}

fn stop_run(
    tx: &Transaction<'_>,
    id: &str,
    caller: Option<&Origin>,
    now: &str,
) -> Result<MutationResult> {
    let details = run_details(tx, id)?;
    authorize_owned_run(tx, id, caller)?;
    let mut result = MutationResult::default();
    if matches!(details.state, RunState::Live | RunState::Stopping) {
        let mut statement=tx.prepare("WITH RECURSIVE owned(id,depth) AS(SELECT ?1,0 UNION ALL SELECT r.id,o.depth+1 FROM run r JOIN owned o ON r.owner_run_id=o.id WHERE r.state IN ('live','stopping')) SELECT id FROM owned ORDER BY depth DESC,id").map_err(sql_error)?;
        result.stop_runs = statement
            .query_map([id], |row| row.get::<_, String>(0))
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?;
        for run in &result.stop_runs {
            super::revisit::drop_pending(tx, run)?;
            tx.execute("UPDATE run SET state='stopping',stop_reason='explicit_stop' WHERE id=?1 AND state='live'",[run]).map_err(sql_error)?;
            tx.execute("UPDATE turn SET generation=generation+1 WHERE run_id=?1 AND state IN ('preparing','running','waiting','recovery_wait')",[run]).map_err(sql_error)?;
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
    if let Some(caller) = caller {
        let owner_live: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND thread_id=?2 AND state='live')",
                params![caller.run_id, caller.thread_id],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if details.owner_run_id.as_deref() != Some(caller.run_id.as_str()) || !owner_live {
            return Err(error(
                ErrorCode::InvalidState,
                "only a live direct owning parent or a human may retry this Run",
            ));
        }
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
        .map_err(sql_error)?
    {
        return Err(error(
            ErrorCode::InvalidState,
            "Run has no recoverable Turn",
        ));
    }
    super::context::record(
        tx,
        &details.thread_id,
        "recovery_resumed",
        actor(caller),
        json!({"run_id":id}),
        now,
    )
    .map_err(sql_error)?;
    Ok(MutationResult {
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
    thread_details(tx, &args.thread_id)?;
    let targets = if args.descendants {
        let mut statement=tx.prepare("WITH RECURSIVE children(id) AS(SELECT ?1 UNION SELECT t.id FROM thread t JOIN children c ON t.parent_thread_id=c.id WHERE t.deleted_at IS NULL) SELECT id FROM children").map_err(sql_error)?;
        statement
            .query_map([&args.thread_id], |row| row.get::<_, String>(0))
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?
    } else {
        vec![args.thread_id.clone()]
    };
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
        super::revisit::delete_thread(tx, id)?;
        tx.execute("UPDATE thread SET deleted_at=?2,revision=revision+1,selection_revision=selection_revision+1,cursor_node_id=NULL,updated_at=?2 WHERE id=?1",params![id,now]).map_err(sql_error)?;
        // Retain the last path/replacement as ownership evidence until payload
        // GC. Deleted Threads are not roots or public selection targets. Removing
        // these rows now makes turn-less summaries/legacy nodes look ownerless.
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [crate::index::IndexKeys::observed(id)],
        )
        .map_err(sql_error)?;
        tx.execute("UPDATE attachment_delivery SET status='cancelled' WHERE thread_id=?1 AND status IN ('accepted','held')",[id]).map_err(sql_error)?;
        tx.execute("UPDATE receipt SET state='cancelled',hold_reason='target_unavailable',updated_at=?2 WHERE target_thread_id=?1 AND state IN ('accepted','held')",params![id,now]).map_err(sql_error)?;
    }
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
    let expired = body.as_deref() == Some(super::receipt::EXPIRED_OUTCOME);
    let outcome: Option<Value> = body
        .map(|body| {
            if expired {
                return Ok(json!({"error":super::receipt::expired_error()}));
            }
            serde_json::from_str(&body)
                .map_err(|_| error(ErrorCode::InvalidState, "operation receipt is malformed"))
        })
        .transpose()?;
    let result = outcome.as_ref().and_then(|v| v.get("result"));
    let mut targets = serde_json::Map::new();
    for key in [
        "thread_id",
        "message_id",
        "run_id",
        "turn_id",
        "execution_id",
        "attachment_id",
        "space_id",
    ] {
        if let Some(value) = result.and_then(|v| v.get(key)).filter(|v| v.is_string()) {
            targets.insert(key.into(), value.clone());
        }
    }
    let mut value = json!({
        "operation_id":id, "instance_id":instance, "created_at":created_at,
        "current_instance":instance == current_instance,
        "state":state, "outcome":outcome,
        "result_uncertain":state == "pending", "result_expired":expired,
        "replay_policy":if state == "pending" { "do_not_reexecute" } else if expired {"operation_expired"} else { "return_saved_outcome" },
        "targets":targets,
    });
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
                |r| Ok((r.get(0)?, r.get(1)?)),
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
    fn actual_correlated_thread_list_seeks_latest_run_once_without_per_run_sort() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        let plans: Vec<String> = db
            .prepare(&format!("EXPLAIN QUERY PLAN {THREAD_LIST_SQL}"))
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
        assert!(
            plans.iter().any(|p| p.contains("runs_by_owner")),
            "{plans:?}"
        );
        // The outer Thread pagination may sort; the scalar Run lookup must use
        // its covering index instead of reading every Run in each Thread.
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
        db.execute("INSERT INTO run(id,thread_id,owner_run_id,state,created_at) VALUES('kid','child','r','live','2026-10-29T00:00:00Z')", []).unwrap();
        verify(ActivityPhase::Background, "completed");
        db.execute("UPDATE run SET state='stopping' WHERE id='kid'", [])
            .unwrap();
        verify(ActivityPhase::Background, "completed");
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
        let no_change = req(Command::ThreadSet(ThreadSetArgs {
            thread_id: Some(thread.into()),
            agents: Some("role.md".into()),
            ..Default::default()
        }));
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
                req(Command::ThreadSet(ThreadSetArgs {
                    thread_id: Some(thread.into()),
                    agents: Some("missing".into()),
                    cwd: Some(root.path().to_str().unwrap().into()),
                    model: Some("must-not-commit".into()),
                    title: Some("not committed".into()),
                    ..Default::default()
                })),
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
                req(Command::ThreadSet(ThreadSetArgs {
                    thread_id: Some(thread.clone()),
                    agents: Some("role.md".into()),
                    ..Default::default()
                })),
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
        let request = req(Command::ThreadSet(ThreadSetArgs {
            thread_id: Some(id.clone()),
            agents: Some("two.md".into()),
            title: Some("must-not-commit".into()),
            ..Default::default()
        }));
        let plan = store
            .prepare_agents(&request, &None)
            .await
            .unwrap()
            .unwrap();
        store
            .execute(
                req(Command::ThreadSet(ThreadSetArgs {
                    thread_id: Some(id.clone()),
                    model: Some("concurrent".into()),
                    ..Default::default()
                })),
                None,
            )
            .await
            .unwrap();
        let saved = details(&store, &id).await;
        let cloned = store.clone();
        let conflict = store
            .database
            .call(move |db| Ok(cloned.execute_transaction(db, &request, None, Some(&plan))))
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
                req(Command::ThreadSet(ThreadSetArgs {
                    thread_id: Some(id.clone()),
                    agents: Some("two.md".into()),
                    ..Default::default()
                })),
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
                req(Command::ThreadSet(ThreadSetArgs {
                    thread_id: Some(id.clone()),
                    cwd: Some("..".into()),
                    ..Default::default()
                })),
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
                        thread_id: Some(target.clone()),
                        text: "synthetic sender fixture".into(),
                        now: false,
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
                            final_answer_only: false,
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
                    thread_id: Some(target),
                    text: "no authority from label".into(),
                    now: false,
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
    fn only_human_or_live_direct_parent_new_run_recovers_original_assigned_input() {
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
            let assigned = assign_message(&tx, "child", caller.as_ref(), "resume").unwrap();
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
                if caller_kind == "foreign" { 0 } else { 1 },
                "{caller_kind}"
            );
            let original: (String, String) = tx
                .query_row(
                    "SELECT run_id,turn_id FROM receipt WHERE id='input'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
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
                sender_label: None,
                thread_id: Some("t".into()),
                text: "urgent".into(),
                now: true,
            }),
        );
        let first = store.execute(request.clone(), None).await.unwrap();
        let accepted: MessageAccepted = serde_json::from_value(first.result.clone()).unwrap();
        assert!(accepted.now);
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
        assert!(replay.now);
        assert_eq!(replay.delivery_status, DeliveryStatus::Accepted);
        db.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod thread_deletion_notification_tests {
    use super::*;
    use crate::{
        app::DataOwner,
        control::{Control, EngineEvent},
    };
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
mod owned_child_notification_tests {
    use super::*;
    use crate::{
        app::DataOwner,
        control::{Control, EngineEvent},
    };
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
    async fn owned_new_run_acceptance_notifies_parent_without_waking_it_or_replaying_effects() {
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
                    sender_label: None,
                    thread_id: Some("child".into()),
                    text: "synthetic delegation".into(),
                    now: false,
                }),
            );
            let result = store.execute(request.clone(), caller.clone()).await;
            if matches!(mode, "stopping-owner" | "rollback") {
                assert_eq!(
                    result.unwrap_err().code,
                    if mode == "rollback" {
                        ErrorCode::SaveFailed
                    } else {
                        ErrorCode::InvalidState
                    }
                );
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
                assert_eq!(
                    first.changed_threads,
                    if mode == "owned" {
                        vec!["parent".to_owned()]
                    } else {
                        vec![]
                    }
                );
                assert_eq!(first.quiet_wake_threads, vec!["child".to_owned()]);
                assert!(first.wake_threads.is_empty() && first.stop_runs.is_empty());
                let owner = db
                    .call(|db| {
                        Ok(db.query_row(
                            "SELECT owner_run_id FROM run WHERE thread_id='child'",
                            [],
                            |row| row.get::<_, Option<String>>(0),
                        )?)
                    })
                    .await
                    .unwrap();
                assert_eq!(owner.as_deref(), (mode == "owned").then_some("parent-run"));
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
                                    sender_label: None,
                                    thread_id: Some("child".into()),
                                    text: "another input".into(),
                                    now: false,
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
    async fn recovery_and_completion_owner_nudges_notify_live_or_stopping_parents_only() {
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
                db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','parent','/','','p','m','now'),('child','child','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent','live','now'); INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES('child-run','child','parent-run','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('child-turn','child','child-run','recovery_wait','now');")?;
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
                control.nudge_owner("child-run").await;
                if parent_state == "completed" {
                    assert!(matches!(
                        events.try_recv(),
                        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                    ));
                } else {
                    assert!(
                        matches!(events.try_recv(),Ok(EngineEvent::Changed {thread_id,recovery:false}) if thread_id=="parent")
                    );
                    assert!(matches!(
                        events.try_recv(),
                        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                    ));
                }
                assert_eq!(
                    nudge.notified().now_or_never().is_some(),
                    parent_state == "live",
                    "only the existing live-parent quiet nudge may run"
                );
            }
            control.live.test_drivers().lock().await.clear();
            owner.shutdown().await.unwrap();
        }
    }
}

#[cfg(test)]
mod driverless_child_stop_notification_tests {
    use super::*;
    use crate::{
        app::DataOwner,
        control::{Control, EngineEvent},
    };
    use futures_util::FutureExt;

    #[tokio::test]
    async fn stopping_an_offscreen_recovery_child_refreshes_parent_only_after_successful_drain() {
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
                    tx.execute("INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES(?1,?2,'parent-run','live',?3)",params![run,thread,format!("2026-10-25T00:00:0{}Z",index+1)])?;
                    tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,'recovery_wait','2026-10-25T00:00:00Z')",params![turn,thread,run])?;
                }
                let oldest=tx.prepare("SELECT id FROM run WHERE owner_run_id='parent-run' AND state IN ('live','stopping') ORDER BY created_at,id LIMIT 3")?.query_map([],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(oldest,["run-0","run-1","run-2"],"target run-3 is outside the bounded display dependencies");
                assert_eq!(tx.query_row("SELECT count(*) FROM run WHERE owner_run_id='parent-run' AND state IN ('live','stopping')",[],|row|row.get::<_,i64>(0))?,4);
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
            assert_eq!(
                parent_notifications,
                usize::from(!fail_drain && parent_state != "completed")
            );
            assert_eq!(child_notifications, 1);
            assert_eq!(errors, usize::from(fail_drain));
            assert!(
                nudge.notified().now_or_never().is_none(),
                "display notification must never nudge the parent model"
            );
            owner.db.call(move |db| {
                let target=db.query_row("SELECT r.state,u.state FROM run r JOIN turn u ON u.run_id=r.id WHERE r.id='run-3'",[],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?;
                assert_eq!(target,if fail_drain {("stopping".into(),"recovery_wait".into())}else{("stopped".into(),"stopped".into())});
                assert_eq!(db.query_row("SELECT count(*) FROM run WHERE owner_run_id='parent-run' AND state IN ('live','stopping')",[],|row|row.get::<_,i64>(0))?,if fail_drain {4}else{3});
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
