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

use super::receipt::Admission;

type Result<T> = std::result::Result<T, ApiError>;

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
    pub instance_id: String,
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
            database,
            config,
            startup_cwd,
            instance_id,
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
            if args.operation_id.is_none() && args.thread_id.is_none() && args.run_id.is_none() {
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
        let store = self.clone();
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
                Ok(store.execute_transaction(connection, &request, caller.as_ref()))
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
            expected_revision,
        )
        .await
    }
    pub(crate) async fn send_draft_content(
        &self,
        operation_id: String,
        args: ThreadCreateArgs,
        thread_id: Option<String>,
        content: crate::attachments::DraftContent,
        files: Vec<crate::attachments::PlannedAttachment>,
        expected_revision: Option<i64>,
    ) -> Result<MutationResult> {
        content.validate(files.len())?;
        if thread_id.is_none() {
            Command::ThreadCreate(args.clone()).validate()?;
        }
        let paths: Vec<Value> = files
            .iter()
            .map(|f| json!({"path":f.path,"kind":f.kind,"managed_id":f.managed_id}))
            .collect();
        let mut intent = json!({"internal_operation":"draft_send","args":args,"thread_id":thread_id,"text":content.text,"files":paths});
        content.fingerprint_positions(&mut intent);
        let fingerprint = hash(serde_json::to_vec(&intent).unwrap());
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
                        if expected_revision.is_some_and(|value| value != revision) {
                            return Err(error(
                                ErrorCode::InvalidState,
                                "Draft attachment selection changed; retry from the retained draft",
                            ));
                        }
                        let result = store.send_content(
                            tx,
                            &MessageSendArgs {
                                thread_id: Some(target),
                                text: content.text.clone(),
                            },
                            None,
                            &operation_id,
                            now,
                            content.new_source_elements(files.len())?,
                        )?;
                        let node = result
                            .result
                            .get("message_id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                error(ErrorCode::InvalidState, "Accepted draft has no source node")
                            })?;
                        crate::attachments::add_tx(
                            tx,
                            &crate::attachments::AddPlan {
                                thread_id: result.result["thread_id"].as_str().unwrap().into(),
                                selection_revision: revision,
                                files,
                            },
                            Some(node),
                        )?;
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
        self.accept_ordered_edit(operation_id, thread, node, content, files, args)
            .await
    }
    pub(crate) async fn accept_ordered_edit(
        &self,
        operation_id: String,
        thread: String,
        node: String,
        content: crate::attachments::DraftContent,
        files: Vec<crate::attachments::PlannedAttachment>,
        args: ThreadCreateArgs,
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
                history::fork_thread(&tx,&ThreadForkArgs{thread_id:thread.clone(),node_id:node.clone(),cwd:args.cwd.clone()},&fork,&now)?;
                let mut path:Vec<String>=tx.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal").map_err(sql_error)?.query_map([&fork],|r|r.get(0)).map_err(sql_error)?.collect::<rusqlite::Result<_>>().map_err(sql_error)?;
                if path.pop().as_deref()!=Some(&node){return Err(error(ErrorCode::InvalidState,"Edit fork boundary changed"));}
                tx.execute("DELETE FROM attachment_selection WHERE thread_id=?1 AND attachment_id IN (SELECT id FROM attachment_ref WHERE source_node_id=?2)",params![fork,node]).map_err(sql_error)?;
                history::select_path(&tx,&fork,1,&path,&now).map_err(storage_error)?;
                if args.connection.is_some()||args.model.is_some()||args.effort.is_some(){store.set_thread(&tx,&ThreadSetArgs{thread_id:Some(fork.clone()),connection:args.connection,model:args.model,effort:args.effort,cwd:None,title:None},None,&now)?;}
                let revision:i64=history::selection_revision(&tx, &fork, history::ThreadScope::Any).map_err(sql_error)?;
                let mut result=store.send_content(&tx,&MessageSendArgs{thread_id:Some(fork),text:content.text.clone()},None,&operation_id,&now,content.new_source_elements(files.len())?)?;
                let message=result.result.get("message_id").and_then(Value::as_str).ok_or_else(||error(ErrorCode::InvalidState,"Accepted edit has no source node"))?;
                crate::attachments::add_tx(&tx,&crate::attachments::AddPlan{thread_id:result.result["thread_id"].as_str().unwrap().into(),selection_revision:revision,files},Some(message))?;
                result.result["source_thread_id"]=json!(thread);result.result["replaced_request_id"]=json!(node);Ok(result)
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
            |tx, now| self.dispatch(tx, &request.command, caller, &request.operation_id, now),
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
                        store.send(
                            tx,
                            &MessageSendArgs {
                                thread_id: Some(thread_id),
                                text,
                            },
                            None,
                            &operation_id,
                            now,
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
                let id = self.create_thread(tx, args, now)?;
                MutationResult::value(thread_details(tx, &id)?)
            }
            Command::ThreadFork(args) => {
                let id = uuid::Uuid::new_v4().to_string();
                history::fork_thread(tx, args, &id, now)?;
                MutationResult::value(thread_details(tx, &id)?)
            }
            Command::ThreadGet(args) => MutationResult::value(thread_details(
                tx,
                &target(args.thread_id.as_deref(), caller)?,
            )?),
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
            Command::OperationGet(args) => MutationResult::value(operation_details(
                tx,
                &args.operation_id,
                &self.instance_id,
            )?),
            Command::RunGet(args) => MutationResult::value(run_details(tx, &args.run_id)?),
            Command::TurnGet(args) => MutationResult::value(turn_details(tx, &args.turn_id)?),
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
                error(
                    ErrorCode::ConfigInvalid,
                    "set a default connection or choose --connection",
                )
            })?;
        let connection = self.config.connections.get(&profile).ok_or_else(|| {
            error(
                ErrorCode::ConfigInvalid,
                "selected connection is not configured",
            )
        })?;
        let model = args
            .model
            .clone()
            .or_else(|| inherited.as_ref().map(|p| p.3.clone()))
            .or_else(|| self.config.default_model.clone())
            .ok_or_else(|| {
                error(
                    ErrorCode::ConfigInvalid,
                    "set a default model or choose --model",
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
            vec![json!({"type":"input_text","text":args.text})],
        )
    }
    fn send_content(
        &self,
        tx: &Transaction<'_>,
        args: &MessageSendArgs,
        caller: Option<&Origin>,
        operation_id: &str,
        now: &str,
        content: Vec<Value>,
    ) -> Result<MutationResult> {
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
        } = assign_message(tx, &id, caller, now)?;
        let message_id = uuid::Uuid::new_v4().to_string();
        let payload = json!({"role":"user","content":content});
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
            run_id: assigned_run,
            turn_id: assigned_turn,
        };
        tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,updated_at,hold_reason,result_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11)",params![receipt_id,operation_id,serde_json::to_string(&caller.as_ref().map(|origin| origin.identity())).unwrap(),id,message_id,accepted.run_id,accepted.turn_id,state,now,reason,serde_json::to_string(&accepted).unwrap()]).map_err(sql_error)?;
        tx.execute(
            "UPDATE thread SET revision=revision+1,updated_at=?2 WHERE id=?1",
            params![id, now],
        )
        .map_err(sql_error)?;
        bump_roots(tx)?;
        let mut result = MutationResult::value(accepted)?;
        if wake {
            result.wake_threads.push(id);
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
            tx.execute("UPDATE receipt SET run_id=?2,turn_id=?3,state='accepted',hold_reason=NULL,updated_at=?4,result_json=json_set(COALESCE(result_json,'{}'),'$.assignment','existing_turn','$.delivery_status','accepted','$.run_id',?2,'$.turn_id',?3) WHERE target_thread_id=?1 AND run_id IS NULL AND turn_id IS NULL AND state='held' AND hold_reason IN ('stopping','previous_instance_interrupted')",params![id,run_id,turn_id,now]).map_err(sql_error)?;
            tx.execute("UPDATE node SET turn_id=?2 WHERE turn_id IS NULL AND id IN (SELECT message_node_id FROM receipt WHERE target_thread_id=?1 AND run_id=?3 AND turn_id=?2)",params![id,turn_id,run_id]).map_err(sql_error)?;
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

fn actor(caller: Option<&Origin>) -> Value {
    caller.map(|value|json!({"kind":"shell","thread_id":value.thread_id,"run_id":value.run_id,"execution_id":value.execution_id})).unwrap_or_else(||json!({"kind":"human"}))
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
    let mut details=connection.query_row("SELECT title,cwd,profile_id,model_id,effort,parent_thread_id FROM thread WHERE id=?1 AND deleted_at IS NULL",[id],|row|Ok(ThreadDetails{context_usage:None,thread_id:id.into(),title:row.get(0)?,cwd:row.get(1)?,connection:row.get(2)?,model:row.get(3)?,effort:row.get(4)?,parent_id:row.get(5)?,run_id:None,turn_id:None,children_count:0,shells_count:0,recovery_reason:None,context:None})).optional().map_err(sql_error)?.ok_or_else(||error(ErrorCode::ThreadNotFound,"Thread not found"))?;
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

/// Display-only fallback. Never rewrite an explicit title or the user's history.
/// The selected path, rather than a Run's latest assistant/tool output, defines
/// the earliest meaningful user text, including on a forked/shared history.
fn thread_display_title(connection: &Connection, id: &str, stored: &str) -> Result<String> {
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
            if payload.get("role").and_then(Value::as_str) != Some("user") {
                continue;
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
                _ => continue,
            };
            let title = bounded_thread_title(&text);
            if !title.is_empty() {
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
    let mut statement=connection.prepare("SELECT id,title,cwd,parent_thread_id,updated_at,(SELECT COALESCE((SELECT state FROM turn WHERE run_id=r.id ORDER BY created_at DESC,id DESC LIMIT 1),r.state) FROM run r WHERE r.thread_id=t.id ORDER BY created_at DESC,id DESC LIMIT 1) FROM thread t WHERE deleted_at IS NULL AND rowid<=?1 AND (?2='' OR updated_at<?2 OR (updated_at=?2 AND id<?3)) ORDER BY updated_at DESC,id DESC LIMIT ?4").map_err(sql_error)?;
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
        items.push(ThreadSummary {
            thread_id: id,
            title,
            cwd,
            parent_id: row.get(3).map_err(sql_error)?,
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
    let row=connection.query_row("SELECT r.target_thread_id,r.state,r.run_id,r.turn_id,r.result_json,t.answer_node_id FROM receipt r LEFT JOIN turn t ON t.id=r.turn_id WHERE message_node_id=?1",[id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?))).optional().map_err(sql_error)?.ok_or_else(||error(ErrorCode::TargetUnavailable,"message not found"))?;
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
    let saved: Option<MessageAccepted> = row.4.and_then(|json| serde_json::from_str(&json).ok());
    let assignment = if row.2.is_none() {
        Assignment::Pending
    } else {
        saved
            .map(|saved| saved.assignment)
            .filter(|assignment| *assignment != Assignment::Pending)
            .unwrap_or(Assignment::ExistingTurn)
    };
    Ok(MessageDetails {
        context: Some(crate::context::for_message(connection, id)?),
        message_id: id.into(),
        thread_id: row.0,
        delivery_status: status,
        assignment,
        run_id: row.2,
        turn_id: row.3,
        answer_node_id: row.5,
    })
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
        context: Some(crate::context::for_run(connection, id)?),
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
    connection.query_row("SELECT run_id,thread_id,state,answer_node_id,recovery_reason_json FROM turn WHERE id=?1",[id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,Option<String>>(4)?))).optional().map_err(sql_error)?.ok_or_else(||error(ErrorCode::TargetUnavailable,"Turn not found")).and_then(|row|Ok(TurnDetails{context:Some(crate::context::for_turn(connection,id)?),turn_id:id.into(),run_id:row.0,thread_id:row.1,state:serde_json::from_value(json!(row.2)).map_err(|_|error(ErrorCode::InvalidState,"unknown saved Turn state"))?,answer_node_id:row.3,recovery_hint:row.4}))
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
    MutationResult::value(Accepted { accepted: true })
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
