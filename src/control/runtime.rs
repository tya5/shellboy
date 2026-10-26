//! Runtime operation deduplication, durable intent, and shell reply ownership.
use super::*;

pub(super) fn encode_result<T: serde::Serialize>(
    result: T,
    code: ErrorCode,
    message: &'static str,
) -> Result<Value, ApiError> {
    serde_json::to_value(result).map_err(|_| ApiError::new(code, message))
}

type RuntimeObserver = tokio::sync::watch::Receiver<Option<Result<Value, ApiError>>>;
fn model_list_error(error: crate::model::ModelError) -> ApiError {
    let code = match error.kind.as_str() {
        "config_invalid" => ErrorCode::ConfigInvalid,
        "input_limit" => ErrorCode::InputLimit,
        "cancelled" => ErrorCode::InvalidState,
        _ => ErrorCode::IoFailed,
    };
    let message = match error.status {
        Some(status) => format!("HTTP {status}: {error}"),
        None => error.to_string(),
    };
    ApiError::new(code, message)
}
#[derive(Clone)]
pub(super) struct RuntimeOperation {
    pub(super) fingerprint: Value,
    pub(super) deferred_shell_stop: Option<String>,
    pub(super) receive: RuntimeObserver,
}

impl Control {
    pub(super) async fn runtime_request(
        self: &Arc<Self>,
        request: Request,
        caller: Option<Origin>,
        disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        let deferred_shell_stop = match (&request.command, caller.as_ref()) {
            (Command::ShellStop(args), Some(origin))
                if origin.execution_id == args.execution_id =>
            {
                Some(args.execution_id.clone())
            }
            _ => None,
        };
        let fingerprint = json!({"command":request.command,"caller":caller});
        let mut operations = self.runtime_operations.lock().await;
        let mut receive = if let Some(operation) = operations.get(&request.operation_id) {
            if operation.fingerprint != fingerprint {
                return Err(ApiError::new(
                    ErrorCode::OperationConflict,
                    "Operation ID is already in use with different arguments",
                ));
            }
            operation.receive.clone()
        } else {
            let (send, receive) = tokio::sync::watch::channel(None);
            operations.insert(
                request.operation_id.clone(),
                RuntimeOperation {
                    fingerprint,
                    deferred_shell_stop: None,
                    receive: receive.clone(),
                },
            );
            let control = self.clone();
            // The owned worker reserves durable intent after releasing admission.
            // Concurrent observers share its result; none can start it twice.
            tokio::spawn(async move {
                let operation_id = request.operation_id.clone();
                let mut owns_stop = false;
                let result = match control
                    .store
                    .reserve_runtime(request.clone(), caller.clone())
                    .await
                {
                    Ok(db::RuntimeReservation::Saved(result)) => result,
                    Ok(db::RuntimeReservation::Pending) => Err(ApiError::new(
                        ErrorCode::InvalidState,
                        "This operation was accepted but its result is uncertain; inspect its receipt without replaying it",
                    )),
                    Err(error) => Err(error),
                    Ok(db::RuntimeReservation::New) => {
                        // Saved results never create a new reply-grace effect.
                        // Publish ownership before the action can save stopping.
                        if let Some(execution) = &deferred_shell_stop {
                            if let Some(operation) = control
                                .runtime_operations
                                .lock()
                                .await
                                .get_mut(&operation_id)
                            {
                                operation.deferred_shell_stop = Some(execution.clone());
                            }
                            owns_stop = true;
                        }
                        let cancellable = request.command.runtime_cancellable();
                        let action = control.runtime_action(request, caller, disconnected);
                        let result = if cancellable {
                            tokio::select! {biased;_=control.stop.cancelled()=>Err(ApiError::new(ErrorCode::InvalidState,"Owner stopped this operation; inspect saved jobs and retry explicitly")),result=action=>result}
                        } else {
                            action.await
                        };
                        if result
                            .as_ref()
                            .err()
                            .is_some_and(|error| error.is_storage_failure())
                        {
                            control.fail_storage().await;
                        }
                        // Pure read results can exceed the frame; store an explicit
                        // bound error rather than an untransportable success. Runtime
                        // mutation builders have certified bounded acknowledgments.
                        let result = match receipt::encode(&operation_id, &result) {
                            Ok(_) => result,
                            Err(error) => Err(error),
                        };
                        match control
                            .store
                            .finalize_runtime(operation_id.clone(), result.clone())
                            .await
                        {
                            Ok(()) => result,
                            Err(error) => {
                                if error.is_storage_failure() {
                                    control.fail_storage().await;
                                }
                                Err(error)
                            }
                        }
                    }
                };
                if result
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.is_storage_failure())
                {
                    control.fail_storage().await;
                }
                let stop_self = owns_stop && result.is_ok();
                let _ = send.send(Some(result));
                if stop_self {
                    control.reply_grace().await;
                }
                // A starting execution can register concurrently. Removing its
                // grace owner and looking up the handle share this lock order
                // with the starter's deferred-stop check, so neither misses it.
                let mut operations = control.runtime_operations.lock().await;
                operations.remove(&operation_id);
                if stop_self && let Some(execution) = deferred_shell_stop {
                    control.live.stop_shell(&execution).await;
                }
                drop(operations);
                control.changed.notify_waiters();
            });
            receive
        };
        drop(operations);
        loop {
            let result = { receive.borrow().clone() };
            if let Some(result) = result {
                return result;
            }
            receive.changed().await.map_err(|_| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "Runtime operation result is uncertain; inspect its saved intent",
                )
            })?;
        }
    }

    async fn runtime_action(
        self: &Arc<Self>,
        request: Request,
        caller: Option<Origin>,
        disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        if self.stop.is_cancelled() && !request.command.runtime_allowed_while_stopping() {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Owner is shutting down",
            ));
        }
        let reader = output::ShellOutputReader::new(
            self.store.database.clone(),
            self.store.instance_id.clone(),
        );
        let is_eof = matches!(&request.command, Command::ShellEof(_));
        let mutation_execution = match &request.command {
            Command::ShellWrite(args) => Some(args.execution_id.clone()),
            Command::ShellEof(args) | Command::ShellStop(args) => Some(args.execution_id.clone()),
            _ => None,
        };
        if let Some(execution) = mutation_execution {
            let origin = caller.clone();
            self.store
                .database
                .call(move |db| {
                    use rusqlite::OptionalExtension;
                    let owner: Option<String> = db
                        .query_row(
                            "SELECT owner_run_id FROM shell_execution WHERE id=?1",
                            [execution],
                            |row| row.get(0),
                        )
                        .optional()?;
                    Ok(match owner {
                        Some(owner) => db::authorize_owned_run(db, &owner, origin.as_ref()),
                        None => Err(ApiError::new(
                            ErrorCode::TargetUnavailable,
                            "Shell execution does not exist",
                        )),
                    })
                })
                .await
                .map_err(storage_error)??;
        }
        match request.command {
            Command::ModelList(args) => self.runtime_models(args, caller, disconnected).await,
            Command::StorageStatus(_) => encode_result(
                self.gc.details().await?,
                ErrorCode::InvalidState,
                "Cannot encode storage usage",
            ),
            Command::StorageCollect(args) => encode_result(
                self.gc
                    .collect(
                        args.category.unwrap_or(StorageCategory::All),
                        chrono::Utc::now(),
                    )
                    .await?,
                ErrorCode::InvalidState,
                "Cannot encode collection result",
            ),
            Command::HistorySearch(args) => encode_result(
                self.index.search_filtered(&args).await?,
                ErrorCode::InvalidState,
                "Cannot encode search result",
            ),
            Command::IndexStatus(args) => encode_result(
                self.index.status(args).await?,
                ErrorCode::InvalidState,
                "Cannot encode index status",
            ),
            Command::IndexPreview(args) => encode_result(
                self.index.preview(args).await?,
                ErrorCode::InvalidState,
                "Cannot encode index preview",
            ),
            Command::IndexBackfill(args) => encode_result(
                self.index.backfill(args).await?,
                ErrorCode::InvalidState,
                "Cannot encode index admission",
            ),
            Command::IndexRebuild(args) => encode_result(
                self.index.rebuild(args).await?,
                ErrorCode::InvalidState,
                "Cannot encode index rebuild admission",
            ),
            Command::IndexRetry(args) => {
                self.index.retry(args).await?;
                Ok(json!({"accepted":true}))
            }
            Command::Wait(args) => {
                self.runtime_wait(request.operation_id, args, caller, disconnected)
                    .await
            }
            Command::ShellGet(args) => encode_result(
                reader.get(&args.execution_id).await?,
                ErrorCode::IoFailed,
                "Cannot encode shell details",
            ),
            Command::ShellRead(args) => encode_result(
                reader.read(args).await?,
                ErrorCode::IoFailed,
                "Cannot encode shell output",
            ),
            Command::ShellWrite(args) => {
                let shell = self
                    .live
                    .clone_shell_handle(&args.execution_id)
                    .await
                    .ok_or_else(|| {
                        ApiError::new(ErrorCode::TargetUnavailable, "Shell is not running")
                    })?;
                encode_result(
                    shell.write(&args.text, &args.encoding).await?,
                    ErrorCode::IoFailed,
                    "Cannot encode shell write result",
                )
            }
            Command::ShellEof(args) | Command::ShellStop(args) => {
                self.signal_shell(args.execution_id, is_eof, caller).await
            }
            _ => Err(ApiError::new(
                ErrorCode::InvalidState,
                "Operation has no runtime adapter",
            )),
        }
    }

    async fn runtime_models(
        &self,
        args: ModelListArgs,
        caller: Option<Origin>,
        disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        // Validate an explicit Thread even when the connection overrides it.
        let thread = args.thread_id.clone().or_else(|| {
            args.connection
                .is_none()
                .then(|| caller.as_ref().map(|o| o.thread_id.clone()))
                .flatten()
        });
        let thread_connection = if let Some(thread) = thread {
            Some(
                self.store
                    .database
                    .call(move |db| {
                        Ok(db::thread_details(db, &thread).map(|details| details.connection))
                    })
                    .await
                    .map_err(storage_error)??,
            )
        } else {
            None
        };
        let connection = args
            .connection
            .or(thread_connection)
            .or_else(|| self.store.config.default_connection.clone())
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::ConfigInvalid,
                    "No default connection is configured",
                )
            })?;
        let profile = self
            .store
            .config
            .connections
            .get(&connection)
            .cloned()
            .ok_or_else(|| {
                ApiError::new(ErrorCode::ConfigInvalid, "Connection is not configured")
            })?;
        let fetch = async {
            let client =
                crate::model::http::HttpModelClient::new(profile).map_err(model_list_error)?;
            client
                .models(self.stop.child_token())
                .await
                .map_err(model_list_error)
        };
        // A read-only catalog request has no target work to keep alive on disconnect.
        let models = tokio::select! { biased;
            _ = disconnected.cancelled() => return Err(ApiError::new(ErrorCode::InvalidState, "Model list caller disconnected")),
            result = fetch => result?,
        };
        encode_result(
            ModelListResult { connection, models },
            ErrorCode::InvalidState,
            "Cannot encode model list",
        )
    }

    async fn runtime_wait(
        self: &Arc<Self>,
        operation_id: String,
        args: WaitArgs,
        caller: Option<Origin>,
        disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        let result = wait::observe(
            self.clone(),
            operation_id,
            args,
            caller.clone(),
            disconnected,
        )
        .await?;
        let failed_target = result
            .pointer("/snapshot/conditions")
            .and_then(Value::as_array)
            .is_some_and(|targets| {
                targets.iter().any(|target| {
                    matches!(
                        target.get("state").and_then(Value::as_str),
                        Some("failed" | "stopped")
                    ) || target
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .is_some_and(|code| code != 0)
                })
            });
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unavailable");
        if (status != "satisfied" || failed_target)
            && let Some(caller) = caller
        {
            let reason = if failed_target {
                "target_failed".to_owned()
            } else {
                status.to_owned()
            };
            self.store
                .database
                .call(move |db| {
                    db.execute(
                        "UPDATE shell_execution SET shell_json=json_set(shell_json,'$.wait_release_outcome',?2) WHERE id=?1",
                        rusqlite::params![caller.execution_id, reason],
                    )?;
                    Ok(())
                })
                .await
                .map_err(storage_error)?;
        }
        Ok(result)
    }

    pub(super) async fn signal_shell(
        &self,
        execution_id: String,
        is_eof: bool,
        caller: Option<Origin>,
    ) -> Result<Value, ApiError> {
        let execution = execution_id.clone();
        let target = self.store.database.call(move |db| {
            let tx = db.transaction()?;
            let changed = if is_eof {
                tx.execute("UPDATE shell_execution SET stdin_state='closed' WHERE id=?1 AND state IN ('starting','running','stopping')",[&execution])?
            } else {
                tx.execute("UPDATE shell_execution SET state='stopping' WHERE id=?1 AND state IN ('starting','running','stopping')",[&execution])?
            };
            let target = if changed != 0 {
                Some(tx.query_row("SELECT r.thread_id FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE s.id=?1",[execution],|r|r.get::<_,String>(0))?)
            } else { None };
            tx.commit()?;
            Ok(target)
        }).await.map_err(storage_error)?;
        if let Some(thread) = target {
            self.changed(&thread);
            let shell = self.live.clone_shell_handle(&execution_id).await;
            if let Some(shell) = shell {
                if is_eof {
                    shell.eof().await?;
                } else if !caller
                    .as_ref()
                    .is_some_and(|origin| origin.execution_id == execution_id)
                {
                    shell.stop();
                }
            }
            Ok(json!({"accepted":true}))
        } else {
            // Repeating EOF/stop against a known terminal execution is a
            // state query; an unknown ID remains an operation error.
            encode_result(
                output::ShellOutputReader::new(
                    self.store.database.clone(),
                    self.store.instance_id.clone(),
                )
                .get(&execution_id)
                .await?,
                ErrorCode::IoFailed,
                "Cannot encode shell details",
            )
        }
    }

    pub(super) async fn reply_grace(&self) {
        let delay = tokio::time::sleep(std::time::Duration::from_millis(
            self.store.config.io.self_reply_grace_ms,
        ));
        tokio::pin!(delay);
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.stop.is_cancelled() || self.admission_closed.load(Ordering::SeqCst) {
                return;
            }
            tokio::select! { biased;
                _ = self.stop.cancelled() => return,
                _ = &mut delay => return,
                _ = changed => {}
            }
        }
    }

    pub(super) async fn shell_start_intent(
        &self,
        execution: &str,
        spawned: bool,
    ) -> Result<(bool, bool), ApiError> {
        let execution = execution.to_owned();
        let (stop,eof,thread) = self.store.database.call(move |db| {
            let tx = db.transaction()?;
            let (state,stdin,run,thread):(String,String,String,String) = tx.query_row("SELECT s.state,s.stdin_state,r.state,r.thread_id FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE s.id=?1",[&execution],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            let stop = !matches!(state.as_str(),"starting"|"running") || run!="live";
            if spawned {
                tx.execute("UPDATE shell_execution SET shell_json=json_set(shell_json,'$.spawned',json('true')),state=CASE WHEN state='starting' THEN 'running' ELSE state END WHERE id=?1",[execution])?;
            } else if stop {
                tx.execute("UPDATE shell_execution SET state='stopped',end_reason='stopped_before_spawn',output_complete=0,ended_at=?2 WHERE id=?1 AND state IN ('starting','stopping')",rusqlite::params![execution,chrono::Utc::now().to_rfc3339()])?;
            }
            tx.commit()?;
            Ok((stop,stdin=="closed",thread))
        }).await.map_err(storage_error)?;
        if spawned || stop {
            self.changed(&thread);
        }
        Ok((stop, eof))
    }

    pub(super) async fn reapply_shell_intent(
        &self,
        execution: &str,
        handle: &ShellHandle,
    ) -> Result<(), ApiError> {
        let (stop, eof) = self.shell_start_intent(execution, true).await?;
        if stop || self.stop.is_cancelled() {
            if self.stop.is_cancelled() {
                handle.stop();
                return Ok(());
            }
            let owners: Vec<_> = self
                .runtime_operations
                .lock()
                .await
                .values()
                .filter(|o| o.deferred_shell_stop.as_deref() == Some(execution))
                .map(|o| o.receive.clone())
                .collect();
            // Snapshot the existing owners once. A failed owner cannot hide a
            // saved external stop or preempt another successful reply's grace.
            // New requests cannot keep this bounded registration check looping.
            for mut owner in owners {
                loop {
                    let result = owner.borrow().clone();
                    if let Some(result) = result {
                        if result.is_ok() {
                            return Ok(());
                        }
                        break;
                    }
                    tokio::select! { biased;
                        _ = self.stop.cancelled() => {handle.stop();return Ok(());},
                        changed = owner.changed() => if changed.is_err() {break;}
                    }
                }
            }
            handle.stop();
        } else if eof {
            handle.eof().await?;
        }
        Ok(())
    }
}
