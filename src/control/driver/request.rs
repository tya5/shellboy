//! Model configuration, borrowed input preparation and bounded stream forwarding.
use super::*;

#[derive(Default)]
pub(super) struct Instructions {
    turn: String,
    body: Option<Arc<str>>,
}
impl Instructions {
    pub(super) fn clear(&mut self) {
        self.body = None;
        self.turn.clear();
    }
    pub(super) fn body(&self) -> Option<&Arc<str>> {
        self.body.as_ref()
    }
    pub(super) async fn refresh(
        &mut self,
        control: &Arc<Control>,
        snapshot: Box<Snapshot>,
        slot: &DriverSlot,
    ) -> Result<Option<Box<Snapshot>>, ApiError> {
        if self.turn == snapshot.turn {
            return Ok(Some(snapshot));
        }
        self.body = match turn_instructions(control, &snapshot).await {
            Ok(body) => body,
            Err(error) => return fail_turn(control, &snapshot, error).await,
        };
        self.turn = snapshot.turn.clone();
        // A known absence is cached too. Observe once per Turn, then reload
        // the append-only observation before the first HTTP request.
        Ok(match next(control, &snapshot.thread, &slot.run_id).await? {
            Next::Ready(snapshot) => Some(snapshot),
            _ => None,
        })
    }
}

pub(super) struct AdmittedRequest {
    pub(super) client: HttpModelClient,
    pub(super) prepared: Arc<crate::attachments::PreparedAttachments>,
    pub(super) request: Value,
    pub(super) attempt: String,
}
pub(super) async fn prepare_attempt(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    instruction_body: Option<&Arc<str>>,
) -> Result<Option<AdmittedRequest>, ApiError> {
    let client = match model_client(control, snapshot) {
        Ok(client) => client,
        Err(error) => return fail_turn(control, snapshot, error).await,
    };
    let mut prepared = match control
        .attachments
        .prepare_at_revision(
            &snapshot.thread,
            slot.stop.clone(),
            false,
            snapshot.selection,
        )
        .await
    {
        Ok(value) => value,
        Err(error) if error.code == ErrorCode::OperationConflict => return Ok(None),
        Err(error) => {
            return fail_turn(control, snapshot, error).await;
        }
    };
    let request = match prepare_request(snapshot, instruction_body, &mut prepared) {
        Ok(request) => request,
        Err(error) => return fail_turn(control, snapshot, error).await,
    };
    let prepared = Arc::new(prepared);
    let attempt = id();
    let started = match admit_attempt(control, snapshot, &attempt, prepared.clone()).await {
        Ok(started) => started,
        Err(error) if error.code == ErrorCode::OperationConflict => return Ok(None),
        Err(error) => return Err(error),
    };
    if !started {
        return Ok(None);
    }
    Ok(Some(AdmittedRequest {
        client,
        prepared,
        request,
        attempt,
    }))
}
pub(super) fn model_client(
    control: &Control,
    snapshot: &Snapshot,
) -> Result<HttpModelClient, ApiError> {
    let Some(connection) = control
        .store
        .config
        .connections
        .get(&snapshot.profile)
        .cloned()
    else {
        let error = ApiError::new(
            ErrorCode::ConfigInvalid,
            "Model connection is not configured",
        )
        .hint("Set default_connection/default_model in TOML or change this Thread model and retry");
        return Err(error);
    };
    if snapshot.model.is_empty() {
        let error = ApiError::new(ErrorCode::ConfigInvalid, "Thread model is not configured");
        return Err(error);
    }
    let client = match HttpModelClient::for_connection(&snapshot.profile, connection) {
        Ok(client) => client,
        Err(error) => {
            let error = ApiError::new(ErrorCode::ConfigInvalid, error.to_string());
            return Err(error);
        }
    };
    Ok(client)
}
pub(super) fn prepare_request(
    snapshot: &Snapshot,
    instruction_body: Option<&Arc<str>>,
    prepared: &mut crate::attachments::PreparedAttachments,
) -> Result<Value, ApiError> {
    let mut input = vec![
        json!({"role":"system","content":[{"type":"input_text","text":"You are Shellboy, a native personal agent. A Thread is a continuing conversation; a Turn is one response interval; a Run owns its shells and child Runs, and may outlive a completed Turn. Execute your own commands by calling the shell function directly. shellboy message send delivers conversation text; it does not execute that text as a shell command. Sending to your own Thread adds input to your current conversation, not a separate agent. A shell tool result's execution_id always identifies the process started by that call, including a CLI process; that process's stdout may itself be JSON. For background work, take the returned execution_id and observe it in a later shell call. Use shellboy shell get --execution ID to check the actual command, cwd, Thread, Run and Turn before diagnosing unexpected output. CLI IDs require named flags, not positional arguments; consult the specific subcommand --help before guessing syntax. Guides are bundled with the executable: use shellboy docs list or shellboy docs search --query TEXT, then shellboy docs get --id ID to read the relevant guide before unfamiliar operations, especially delegation. No repository path is required. Use shellboy --help and shellboy history commands to inspect state, send messages, create children, wait, attach files and retrieve original history. Report actual results and errors. Commands share filesystem access; histories and cwd are per Thread."}]}),
    ];
    if let Some(body) = instruction_body {
        input.push(json!({"role":"developer","content":[{"type":"input_text","text":&**body}]}));
    }
    if let Some(replay) = &snapshot.input {
        crate::control::attachment_input::bind(replay, prepared, &mut input)?;
    }
    input.push(json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy context v1: thread={} run={} turn={} cwd={} connection={} model={} effort={:?}; AGENTS reference={}",snapshot.thread,snapshot.run,snapshot.turn,snapshot.cwd.display(),snapshot.profile,snapshot.model,snapshot.effort,snapshot.agents.display())}]}));
    Ok(model::response_request(
        &snapshot.model,
        input,
        snapshot.effort.as_deref(),
        true,
    ))
}
// Preserve the client's existing bounded partial-text/items/raw recovery type.
#[allow(clippy::result_large_err)]
pub(super) async fn stream_response(
    control: &Control,
    thread: &str,
    attempt: &str,
    client: &HttpModelClient,
    request: Value,
    stop: &CancellationToken,
) -> Result<model::ModelResponse, model::http::AttemptFailure> {
    let (send, mut receive) = mpsc::channel::<model::http::ObservedEvent>(32);
    let mut stream_chars = 0usize;
    let mut last_receive_notice: Option<std::time::Instant> = None;
    let response = client.response_observed(request, stop.clone(), Some(send));
    tokio::pin!(response);
    loop {
        tokio::select! {
            result=&mut response=>break result,
            Some(event)=receive.recv()=>{
                let (event, observed_at) = match event {
                    model::http::ObservedEvent::Received(at) => (None, at),
                    model::http::ObservedEvent::Model { value, received_at } => (Some(value), received_at),
                };
                // Bound additional UI traffic, including byte-sized keepalives.
                // A receipt after a long silence is always forwarded immediately.
                if last_receive_notice.is_none_or(|last| observed_at.saturating_duration_since(last) >= std::time::Duration::from_secs(1)) {
                    last_receive_notice = Some(observed_at);
                    let _ = control.events.send(EngineEvent::ModelReceive {
                        thread_id: thread.into(),
                        attempt_id: attempt.into(),
                        observed_at,
                    });
                }
                let Some(event) = event else { continue; };
                let kind=model::classify_stream_event(&event);
                let reasoning=kind==model::StreamEventKind::ReasoningDelta;
                if matches!(kind,model::StreamEventKind::TextDelta|model::StreamEventKind::ReasoningDelta) && let Some(delta)=event.get("delta").and_then(Value::as_str){let stream_char_offset=stream_chars;stream_chars+=delta.chars().count();let _=control.events.send(EngineEvent::Stream{thread_id:thread.into(),node_id:attempt.into(),text:delta.into(),reasoning,stream_char_offset});}
            }
        }
    }
}

impl Snapshot {
    pub(super) fn admit_attempt_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        attempt_id: &str,
        attempt_attachments: &crate::attachments::PreparedAttachments,
    ) -> Result<bool, crate::storage::StorageError> {
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id JOIN thread t ON t.id=u.thread_id WHERE u.id=?1 AND u.generation=?2 AND u.state IN ('preparing','running','waiting') AND r.state='live' AND t.selection_revision=?3 AND t.profile_revision=?4)",params![self.turn,self.generation,self.selection,self.profile_revision],|r|r.get(0))?;
        if !valid {
            return Ok(false);
        }
        if tx.execute("UPDATE turn SET state='running' WHERE id=?1 AND generation=?2 AND state IN ('preparing','running','waiting')",params![self.turn,self.generation])? != 1 {
            return Err(ApiError::new(ErrorCode::OperationConflict,"Turn changed before attempt admission").into());
        }
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,effort,reasoning_epoch,state,input_manifest_json,started_at)VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'inflight',?9,?10)",params![attempt_id,self.turn,self.generation,self.selection,self.profile,self.model,self.effort,self.epoch,json!({"receipts":self.receipts,"events":self.events,"attachments":attempt_attachments.manifest}).to_string(),now()])?;
        crate::attachments::adopt_prepared_tx(tx, attempt_attachments, attempt_id)
            .map_err(crate::storage::StorageError::from)?;
        Ok(true)
    }
}
