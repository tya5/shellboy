//! Runtime operation deduplication, durable intent, and shell reply ownership.
use super::*;

pub(super) fn encode_result<T: serde::Serialize>(
    result: T,
    code: ErrorCode,
    message: &'static str,
) -> Result<Value, ApiError> {
    serde_json::to_value(result).map_err(|_| ApiError::new(code, message))
}

#[derive(Clone, Copy)]
pub(super) enum ShellStopIntent {
    Explicit,
    OwnerShutdown,
    ScheduleTimeout,
    ScheduleDeletion,
    ThreadDeletion,
}

pub(super) fn record_shell_stop(
    tx: &rusqlite::Transaction<'_>,
    execution: &str,
    intent: ShellStopIntent,
) -> rusqlite::Result<bool> {
    let reason = match intent {
        ShellStopIntent::OwnerShutdown => "owner_shutdown",
        ShellStopIntent::Explicit => "explicit_stop",
        ShellStopIntent::ScheduleTimeout => "schedule_timeout",
        ShellStopIntent::ScheduleDeletion => "schedule_deleted",
        ShellStopIntent::ThreadDeletion => "thread_deleted",
    };
    let saved = tx.execute("UPDATE shell_execution SET state='stopping',end_reason=?2 WHERE id=?1 AND state IN ('starting','running')",rusqlite::params![execution,reason])?;
    if saved != 0 && matches!(intent, ShellStopIntent::Explicit) {
        let definition: Option<String> = tx.query_row(
            "SELECT schedule_id FROM shell_execution WHERE id=?1",
            [execution],
            |r| r.get(0),
        )?;
        if let Some(id) = definition {
            super::schedule::disable_services(tx, &id, false)?;
        }
    }
    Ok(saved != 0
        || tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM shell_execution WHERE id=?1 AND state='stopping')",
            [execution],
            |r| r.get::<_, bool>(0),
        )?)
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
    let mut details = json!({"failure_kind":error.kind,"http_status":error.status});
    let mut reported = ApiError::new(code, message);
    if let Some(cause) = error.transport_cause {
        details["transport_cause"] = json!(cause);
        reported = reported.hint(cause.next_action());
    }
    reported.details(details)
}
pub(super) struct Settlement(Vec<String>);
#[derive(Clone)]
pub(super) struct RuntimeOperation {
    pub(super) fingerprint: Value,
    pub(super) deferred_shell_stop: Option<String>,
    pub(super) receive: RuntimeObserver,
}

impl Control {
    pub(super) async fn runtime_observation(
        self: &Arc<Self>,
        request: Request,
        caller: Option<Origin>,
        disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        debug_assert!(!request.command.requires_receipt());
        if !request.command.runtime_cancellable() {
            // IndexStatus is an already queued finite DB read, as before.
            return self.runtime_action(request, caller, disconnected).await;
        }
        let cancelled = disconnected.clone();
        // The outer admitted request owns this bounded observation. Existing
        // readers retain their pin/file-worker cleanup when the future drops.
        tokio::select! { biased;
            _ = self.stop.cancelled() => Err(ApiError::new(ErrorCode::InvalidState,
                "Owner stopped this observation; no operation receipt was created")
                .hint("Repeat the observation after the owner is available; a semantic search may incur another query embedding charge")),
            _ = cancelled.cancelled() => Err(ApiError::new(ErrorCode::InvalidState,
                "Observation caller disconnected; no operation receipt was created")
                .hint("Repeat the observation for fresh state; a semantic search may incur another query embedding charge")),
            result = self.runtime_action(request, caller, disconnected) => result,
        }
    }

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
                let mut settlement = None;
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
                    Ok(db::RuntimeReservation::New(_)) => {
                        // The pending receipt is durable before external work.
                        let target = control.settlement(&request.command, caller.as_ref()).await;
                        control.announce(&target);
                        settlement = Some(target);
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
                        if matches!(
                            request.command,
                            Command::ThreadDelete(_)
                                | Command::ThreadDeleteSet(_)
                                | Command::ScheduleRemove(_)
                                | Command::ServiceRemove(_)
                                | Command::ServiceStop(_)
                        ) {
                            super::schedule::delete(&control, request).await
                        } else {
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
                match &settlement {
                    Some(target) => control.announce(target),
                    None => control.changed.notify_waiters(),
                }
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

    /// Where a runtime receipt's reservation and settlement are announced: the
    /// Thread its saved target belongs to. State marks come from their owners.
    async fn settlement(&self, command: &Command, caller: Option<&Origin>) -> Settlement {
        let lookup = |sql: String, id: String| async move {
            use rusqlite::OptionalExtension;
            self.store
                .database
                .call(move |db| {
                    Ok(db
                        .query_row(&sql, [id], |row| row.get::<_, Option<String>>(0))
                        .optional()?
                        .flatten())
                })
                .await
                .map_err(storage_error)
        };
        let shell = |id: &str| {
            lookup(
                format!(
                    "SELECT {} {} WHERE s.id=?1",
                    output::SHELL_THREAD_ID,
                    output::SHELL_THREAD_JOIN
                ),
                id.to_owned(),
            )
        };
        let schedule = |id: &str| {
            lookup(
                "SELECT thread_id FROM schedule WHERE id=?1".into(),
                id.to_owned(),
            )
        };
        let thread = match command {
            Command::ThreadDelete(args) => return Settlement(vec![args.thread_id.clone()]),
            Command::ThreadDeleteSet(args) => return Settlement(args.thread_ids.clone()),
            Command::ShellWrite(args) => shell(&args.execution_id).await,
            Command::ShellEof(args) | Command::ShellStop(args) | Command::ShellBackground(args) => {
                shell(&args.execution_id).await
            }
            Command::TurnInterrupt(args) => {
                lookup(
                    "SELECT thread_id FROM turn WHERE id=?1".into(),
                    args.turn_id.clone(),
                )
                .await
            }
            Command::ScheduleRemove(args) => schedule(&args.schedule_id).await,
            Command::ServiceStop(args) | Command::ServiceRemove(args) => {
                schedule(&args.service_id).await
            }
            // A wait belongs to its caller; other runtime receipts name no Thread.
            _ => Ok(caller.map(|origin| origin.thread_id.clone())),
        };
        match thread {
            Ok(thread) => Settlement(thread.into_iter().collect()),
            Err(error) => {
                if error.is_storage_failure() {
                    self.fail_storage().await;
                }
                Settlement(Vec::new())
            }
        }
    }
    fn announce(&self, settlement: &Settlement) {
        if settlement.0.is_empty() {
            self.changed.notify_waiters();
        }
        for thread in &settlement.0 {
            self.changed(thread);
        }
    }

    async fn runtime_action(
        self: &Arc<Self>,
        request: Request,
        caller: Option<Origin>,
        disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        if matches!(&request.command, Command::Status(a) if a.memory) {
            return self.memory_snapshot(disconnected).await;
        }
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
        // No authority check (#256): any caller may write to, EOF or stop any shell,
        // but an unknown execution is still an operation error.
        let mutation_execution = match &request.command {
            Command::ShellWrite(args) => Some(args.execution_id.clone()),
            Command::ShellEof(args) | Command::ShellStop(args) => Some(args.execution_id.clone()),
            _ => None,
        };
        if let Some(execution) = mutation_execution {
            let exists = self
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM shell_execution WHERE id=?1)",
                        [execution],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .await
                .map_err(storage_error)?;
            if !exists {
                return Err(ApiError::new(
                    ErrorCode::TargetUnavailable,
                    "Shell execution does not exist",
                ));
            }
        }
        match request.command {
            Command::ShellBackground(args) => {
                use rusqlite::OptionalExtension;
                let execution = args.execution_id;
                let id = execution.clone();
                let row = self.store.database.call(move |db| {
                    Ok(db.query_row("SELECT r.thread_id,r.id,s.state,r.state,u.state FROM shell_execution s LEFT JOIN run r ON r.id=s.owner_run_id LEFT JOIN turn u ON u.id=s.turn_id WHERE s.id=?1", [id], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, Option<String>>(4)?))).optional()?)
                }).await.map_err(storage_error)?;
                let Some((thread, run, state, run_state, turn_state)) = row else {
                    return Err(ApiError::new(
                        ErrorCode::TargetUnavailable,
                        "Execution was not found",
                    )
                    .details(json!({"reason":"execution_not_found","execution_id":execution})));
                };
                match (
                    thread,
                    run,
                    state.as_str(),
                    run_state.as_deref(),
                    turn_state.as_deref(),
                ) {
                    (
                        Some(thread),
                        Some(run),
                        "running",
                        Some("live"),
                        Some("running" | "waiting"),
                    ) => self.background(&thread, Some((&run, &execution))).await,
                    _ => Err(super::background_refusal("not_running_foreground")),
                }
            }
            Command::TurnInterrupt(args) => self.interrupt_turn(args.turn_id).await,
            Command::ModelList(args) => self.runtime_models(args, caller, disconnected).await,
            Command::ServiceEndpoint(args) => encode_result(
                super::service_endpoint::read(self, args).await?,
                ErrorCode::IoFailed,
                "Cannot encode service endpoint",
            ),
            Command::ArtifactRead(args) => encode_result(
                crate::artifacts::read(
                    &request.operation_id,
                    &self.store.database,
                    &self.attachments.workers,
                    self.store.config.limits.attachment_file_bytes,
                    self.store.config.io.file_prepare_timeout_ms,
                    self.stop.child_token(),
                    args,
                )
                .await?,
                ErrorCode::IoFailed,
                "Cannot encode artifact content",
            ),
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
            Command::ConfigGet(_) => self.config_get(disconnected).await,
            Command::ConfigSet(args) => self.config_set(args, caller.as_ref()).await,
            Command::CredentialsSet(args) => self.credentials_set(args, caller.as_ref()).await,
            Command::CredentialsClear(args) => self.credentials_clear(args, caller.as_ref()).await,
            Command::ConnectionTest(args) => self.connection_test(args, disconnected).await,
            Command::HistorySearch(args) => encode_result(
                self.index.search_report(&args, disconnected).await?,
                ErrorCode::InvalidState,
                "Cannot encode search result",
            ),
            Command::IndexProbe(args) => encode_result(
                self.index.probe(args).await?,
                ErrorCode::InvalidState,
                "Cannot encode index probe",
            ),
            Command::IndexShrink(args) => encode_result(
                self.index.shrink(args).await?,
                ErrorCode::InvalidState,
                "Cannot encode index conversion",
            ),
            Command::IndexRemove(args) => encode_result(
                self.index.remove_space(args).await?,
                ErrorCode::InvalidState,
                "Cannot encode index removal",
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
            Command::EventsWait(args) => self.runtime_events_wait(args, disconnected).await,
            Command::Wait(args) => {
                self.runtime_wait(request.operation_id, args, caller, disconnected)
                    .await
            }
            Command::ShellGet(args) => {
                let mut result = encode_result(
                    reader.get(&args.execution_id).await?,
                    ErrorCode::IoFailed,
                    "Cannot encode shell details",
                )?;
                self.live
                    .project_shell_foreground(std::iter::once(&mut result))
                    .await;
                self.project_shell_waits(std::iter::once(&mut result))
                    .await?;
                Ok(result)
            }
            Command::ShellRead(args) => encode_result(
                reader.read(args).await?,
                ErrorCode::IoFailed,
                "Cannot encode shell output",
            ),
            Command::AttachmentReload(args) => {
                let cancel = disconnected.child_token();
                let _cancel_on_drop = cancel.clone().drop_guard();
                // Let the worker take ownership of selection pins even if the caller disconnects.
                let owner = self.clone();
                let prepared = tokio::spawn(async move {
                    owner
                        .attachments
                        .prepare(&args.thread_id, cancel, true)
                        .await
                })
                .await
                .map_err(|_| {
                    ApiError::new(ErrorCode::InvalidState, "Attachment reload worker failed")
                })??;
                encode_result(
                    crate::protocol::AttachmentReloadResult {
                        thread_id: prepared.thread_id,
                        selection_revision: prepared.selection_revision,
                        attachments: prepared.manifest,
                        notices: prepared.notices,
                        would_exclude: prepared.excluded_ids,
                        sent: false,
                    },
                    ErrorCode::IoFailed,
                    "Cannot encode attachment reload",
                )
            }
            Command::ShellWrite(args) => {
                let shell = match self.live.clone_shell_handle(&args.execution_id).await {
                    Some(shell) => shell,
                    None => {
                        reader.get(&args.execution_id).await?;
                        return Err(crate::shell::stdin_closed_error());
                    }
                };
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
        let config = &self.store.config.current();
        let clients = self.store.clients.snapshot();
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
            .or_else(|| clients.default_connection.clone())
            .ok_or_else(|| {
                crate::config_setup::enrich(
                    config,
                    None,
                    Some("catalog"),
                    ApiError::new(
                        ErrorCode::ConfigInvalid,
                        "No default connection is configured",
                    ),
                )
            })?;
        let profile = clients.connections.get(&connection).ok_or_else(|| {
            crate::config_setup::enrich(
                config,
                Some(&connection),
                Some("catalog"),
                ApiError::new(ErrorCode::ConfigInvalid, "Connection is not configured"),
            )
        })?;
        let fetch = async {
            let client = clients.get(&connection).map_err(|error| {
                crate::config_setup::enrich(
                    config,
                    Some(&connection),
                    Some("catalog"),
                    model_list_error(error),
                )
            })?;
            client
                .models(self.stop.child_token())
                .await
                .map_err(|error| {
                    crate::config_setup::enrich(
                        config,
                        Some(&connection),
                        Some("catalog"),
                        model_list_error(error),
                    )
                })
        };
        // A read-only catalog request has no target work to keep alive on disconnect.
        let mut models = tokio::select! { biased;
            _ = disconnected.cancelled() => return Err(ApiError::new(ErrorCode::InvalidState, "Model list caller disconnected")),
            result = fetch => result?,
        };
        profile.models.retain(&mut models);
        encode_result(
            ModelListResult {
                api: profile.api.as_str().into(),
                unavailable_features: profile.api.unavailable_features(),
                connection,
                models,
            },
            ErrorCode::InvalidState,
            "Cannot encode model list",
        )
    }

    /// Receipt-free long-poll over the in-memory change clock; never reads the DB.
    /// Lost-wakeup discipline as in `wait`: enable the notification, then read the clock.
    async fn runtime_events_wait(
        self: &Arc<Self>,
        args: EventsWaitArgs,
        _disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        // Shutdown and caller disconnect cancel this future in `runtime_observation`.
        use super::change_clock::Poll;
        let include_text = args.include_text;
        let include_reasoning_activity = args.include_reasoning_activity;
        let include_output = args.include_output;
        struct Page {
            status: EventsWaitStatus,
            seq: u64,
            changes: Vec<(String, u64)>,
            more: bool,
            marks: [bool; 3],
            text: Vec<crate::protocol::EventLiveText>,
            reasoning_activity: Vec<crate::protocol::EventReasoningActivity>,
            attempt_ends: Vec<crate::protocol::EventAttemptEnd>,
            recovery_transitions: Vec<crate::protocol::EventRecoveryTransition>,
            output: Vec<crate::protocol::EventOutput>,
        }
        let result = |page: Page| {
            let cursor = EventCursor {
                v: 1,
                instance_id: self.store.instance_id.clone(),
                seq: page.seq,
            }
            .encode();
            let [config_changed, index_changed, retention_changed] = page.marks;
            encode_result(
                EventsWaitResult {
                    status: page.status,
                    cursor,
                    changes: page
                        .changes
                        .into_iter()
                        .map(|(thread_id, seq)| EventChange { thread_id, seq })
                        .collect(),
                    more: page.more,
                    config_changed,
                    index_changed,
                    retention_changed,
                    recovery_transitions: page.recovery_transitions,
                    live_text: include_text.then_some(page.text),
                    reasoning_activity: page.reasoning_activity,
                    attempt_ends: include_text.then_some(page.attempt_ends),
                    output: include_output.then_some(page.output),
                },
                ErrorCode::InvalidState,
                "Cannot encode events result",
            )
        };
        // Baselines and resynchronisation carry no change entries, marks or output.
        let snapshot = |status, head, text, reasoning_activity| Page {
            status,
            seq: head,
            changes: vec![],
            more: false,
            marks: [false; 3],
            text,
            reasoning_activity,
            attempt_ends: vec![],
            recovery_transitions: vec![],
            output: vec![],
        };
        let Some(token) = args.after.as_deref() else {
            let (head, text, activity) = self.change_clock.snapshot(
                &args.thread_ids,
                include_text,
                include_reasoning_activity,
            );
            return result(snapshot(EventsWaitStatus::Baseline, head, text, activity));
        };
        let after = EventCursor::decode(token)?;
        if after.instance_id != self.store.instance_id {
            let (head, text, activity) = self.change_clock.snapshot(
                &args.thread_ids,
                include_text,
                include_reasoning_activity,
            );
            return result(snapshot(
                EventsWaitStatus::ResyncRequired,
                head,
                text,
                activity,
            ));
        }
        let deadline = tokio::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(args.timeout_ms));
        use super::wait::wakes::Key;
        let threads: Vec<_> = if args.thread_ids.is_empty() {
            vec![None]
        } else {
            args.thread_ids.iter().cloned().map(Some).collect()
        };
        let mut keys = vec![Key::Marks];
        for thread in threads {
            keys.push(Key::Changed(thread.clone()));
            if include_text {
                keys.extend([Key::Live(thread.clone()), Key::AttemptEnd(thread.clone())]);
            }
            if include_output {
                keys.push(Key::Output(thread.clone()));
            }
            if include_reasoning_activity {
                keys.push(Key::Reasoning(thread));
            }
        }
        let subscription = self.waits.keyed.subscribe(keys);
        subscription.cursor(after.seq, include_text);
        loop {
            let notified = subscription.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.change_clock.poll(
                after.seq,
                &args.thread_ids,
                include_text,
                include_reasoning_activity,
                include_output,
            ) {
                None => {
                    return Err(ApiError::new(
                        ErrorCode::InvalidArgument,
                        "events cursor is ahead of this owner; restart without --after",
                    ));
                }
                Some(Poll::Resync {
                    head,
                    text,
                    reasoning_activity,
                }) => {
                    return result(snapshot(
                        EventsWaitStatus::ResyncRequired,
                        head,
                        text,
                        reasoning_activity,
                    ));
                }
                Some(Poll::Ready {
                    changes,
                    more,
                    cursor,
                    text,
                    reasoning_activity,
                    reasoning_changed,
                    recovery_transitions,
                    config_changed,
                    index_changed,
                    retention_changed,
                    output,
                    attempt_ends,
                }) => {
                    let marks = [config_changed, index_changed, retention_changed];
                    let status = if !changes.is_empty()
                        || marks.contains(&true)
                        || !output.is_empty()
                        || !recovery_transitions.is_empty()
                    {
                        Some(EventsWaitStatus::Changed)
                    } else if reasoning_changed
                        || !attempt_ends.is_empty()
                        || text.iter().any(|attempt| !attempt.deltas.is_empty())
                    {
                        Some(EventsWaitStatus::Text)
                    } else if deadline.is_some_and(|at| tokio::time::Instant::now() >= at) {
                        Some(EventsWaitStatus::Timeout)
                    } else {
                        None
                    };
                    if let Some(status) = status {
                        let changed = status == EventsWaitStatus::Changed;
                        return result(Page {
                            status,
                            seq: cursor,
                            changes: if changed { changes } else { vec![] },
                            more: changed && more,
                            marks: if changed { marks } else { [false; 3] },
                            text,
                            reasoning_activity,
                            attempt_ends,
                            recovery_transitions,
                            output,
                        });
                    }
                }
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = async { match deadline { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => {}
            }
        }
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
        let publications = self.publications.clone();
        let target = self.store.database.call(move |db| {
            let tx = db.transaction()?;
            let changed = if is_eof {
                tx.execute("UPDATE shell_execution SET stdin_state='closed' WHERE id=?1 AND stdin_state='open' AND state IN ('starting','running','stopping')",[&execution])?
            } else {
                usize::from(record_shell_stop(&tx,&execution,ShellStopIntent::Explicit)?)
            };
            let target = if changed != 0 {
                Some(tx.query_row("SELECT COALESCE(r.thread_id,q.thread_id,s.detached_thread_id) FROM shell_execution s LEFT JOIN run r ON r.id=s.owner_run_id LEFT JOIN schedule q ON q.id=s.schedule_id WHERE s.id=?1",[&execution],|r|r.get::<_,String>(0))?)
            } else { None };
            if !is_eof && target.is_some() {
                publications.commit_revoke(tx, [execution.as_str()])?;
            } else {
                tx.commit()?;
            }
            Ok(target)
        }).await.map_err(storage_error)?;
        if let Some(thread) = target {
            self.changed(&thread);
            let shell = self.live.clone_shell_handle(&execution_id).await;
            if let Some(shell) = shell {
                if is_eof {
                    shell.recorded_eof().await?;
                } else if !caller
                    .as_ref()
                    .is_some_and(|origin| origin.execution_id == execution_id)
                {
                    shell.stop();
                }
            }
            if is_eof {
                encode_result(
                    Accepted { accepted: true },
                    ErrorCode::IoFailed,
                    "Cannot encode shell EOF result",
                )
            } else {
                encode_result(
                    crate::protocol::ShellStopResult::Accepted(Accepted { accepted: true }),
                    ErrorCode::IoFailed,
                    "Cannot encode shell stop result",
                )
            }
        } else {
            // Preserve unknown-ID lookup errors, but a known closed stdin
            // rejects EOF instead of treating it as a terminal state query.
            if is_eof {
                output::ShellOutputReader::new(
                    self.store.database.clone(),
                    self.store.instance_id.clone(),
                )
                .get(&execution_id)
                .await?;
                return Err(crate::shell::stdin_closed_error());
            }
            let details = output::ShellOutputReader::new(
                self.store.database.clone(),
                self.store.instance_id.clone(),
            )
            .get(&execution_id)
            .await?;
            encode_result(
                crate::protocol::ShellStopResult::Finished(Box::new(details)),
                ErrorCode::IoFailed,
                "Cannot encode shell stop result",
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
            if self.stop.is_cancelled() || self.admission.is_closed() {
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
        publication: Option<std::sync::Weak<super::service_endpoint::Publication>>,
    ) -> Result<(bool, bool), ApiError> {
        let execution = execution.to_owned();
        let registry = self.publications.clone();
        let wait_wakes = self.waits.keyed.clone();
        let (stop,eof,thread) = self.store.database.call(move |db| {
            let tx = db.transaction()?;
            let (state,stdin,run,thread):(String,String,Option<String>,String) = tx.query_row("SELECT s.state,s.stdin_state,r.state,COALESCE(r.thread_id,q.thread_id,s.detached_thread_id) FROM shell_execution s LEFT JOIN run r ON r.id=s.owner_run_id LEFT JOIN schedule q ON q.id=s.schedule_id WHERE s.id=?1",[&execution],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            let stop = !matches!(state.as_str(),"starting"|"running") || run.as_deref().is_some_and(|state| state!="live");
            let mut terminal = false;
            if spawned {
                tx.execute("UPDATE shell_execution SET shell_json=json_set(shell_json,'$.spawned',json('true')),state=CASE WHEN state='starting' AND NOT ?2 THEN 'running' ELSE state END WHERE id=?1",rusqlite::params![&execution, stop])?;
            } else if stop && tx.execute("UPDATE shell_execution SET state='stopped',end_reason=COALESCE(end_reason,'stopped_before_spawn'),output_complete=0,ended_at=?2 WHERE id=?1 AND state IN ('starting','stopping')",rusqlite::params![&execution,chrono::Utc::now().to_rfc3339()])? != 0 {
                super::schedule::retire_oneshot(&tx,&execution)?;
                terminal = true;
            }
            tx.commit()?;
            if spawned && !stop && let Some(publication) = publication.and_then(|publication| publication.upgrade()) {
                registry.register(execution.clone(), &publication);
            }
            if terminal { wait_wakes.publish(super::wait::wakes::Key::shell_terminal(execution)); }
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
        publication: Option<std::sync::Weak<super::service_endpoint::Publication>>,
    ) -> Result<(), ApiError> {
        let (stop, eof) = self
            .shell_start_intent(execution, true, publication)
            .await?;
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
            handle.reapply_eof().await?;
        }
        Ok(())
    }
}
