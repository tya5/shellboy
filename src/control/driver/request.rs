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
    snapshot: &mut Snapshot,
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
    snapshot.cache_diagnostics_enabled = client.cache_diagnostics_enabled();
    let request = match prepare_request(snapshot, instruction_body, &mut prepared) {
        Ok(request) => request,
        Err(error) => return fail_turn(control, snapshot, error).await,
    };
    // Count the actual serialized input, including system/developer instructions
    // and attachment projections. This is a Unicode scalar count, not UTF-8
    // bytes or a tokenizer claim.
    let input_characters = match client.input_characters(&request) {
        Ok(count) => count,
        Err(error) => {
            return fail_turn(
                control,
                snapshot,
                ApiError::new(ErrorCode::InvalidState, error.to_string()),
            )
            .await;
        }
    };
    // Record selected replay size separately from fixed instructions and files.
    let history_characters = snapshot.input.as_ref().map_or(2, |replay| {
        json!(replay.materialize()).to_string().chars().count() as u64
    });
    if slot.stop.is_cancelled() {
        return Ok(None);
    }
    let prepared = Arc::new(prepared);
    let attempt = id();
    let started = match admit_attempt(
        control,
        snapshot,
        &attempt,
        prepared.clone(),
        input_characters,
        history_characters,
    )
    .await
    {
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
    if !control
        .store
        .config
        .connections
        .contains_key(&snapshot.profile)
    {
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
    let client = match control.store.clients.get(&snapshot.profile) {
        Ok(client) => client,
        Err(error) => {
            let error = ApiError::new(ErrorCode::ConfigInvalid, error.to_string());
            return Err(error);
        }
    };
    Ok(client)
}
fn instruction_message(source: &std::path::Path, body: &str) -> Result<String, ApiError> {
    use std::fmt::Write;
    let source = source.to_str().ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidState,
            "Instruction snapshot source path is not UTF-8",
        )
    })?;
    let quoted = serde_json::to_string(source).expect("a string serializes as JSON");
    let mut safe = String::with_capacity(quoted.len());
    // JSON escapes C0 controls; also cover DEL/C1 and Unicode line separators.
    for ch in quoted.chars() {
        if ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}') {
            write!(safe, "\\u{:04x}", u32::from(ch)).expect("writing to String cannot fail");
        } else {
            safe.push(ch);
        }
    }
    Ok(format!(
        "Working instructions from {safe} (user-editable; re-read at each new Turn start; same-Turn retries use the saved snapshot):\n{body}"
    ))
}

/// The fields of the `Shellboy context v1` line.
pub(crate) struct ContextLine<'a> {
    pub thread: &'a str,
    pub run: &'a str,
    pub turn: &'a str,
    pub cwd: &'a std::path::Path,
    pub profile: &'a str,
    pub model: &'a str,
    pub effort: Option<&'a str>,
    pub agents: &'a std::path::Path,
}
/// Leading system/developer items, before the replayed conversation.
pub(crate) fn leading_items(
    agents: &std::path::Path,
    instruction_body: Option<&str>,
) -> Result<Vec<Value>, ApiError> {
    let mut input = vec![
        json!({"role":"system","content":[{"type":"input_text","text":"You are Shellboy, a native personal agent. A Thread is a continuing conversation; a Turn is one response interval; a Run owns its shells and child Runs, and may outlive a completed Turn. Execute your own commands by calling the shell function directly. shellboy message send delivers conversation text; it does not execute that text as a shell command. Sending to your own Thread adds input to your current conversation, not a separate agent. A shell tool result's execution_id always identifies the process started by that call, including a CLI process; that process's stdout may itself be JSON. For background work, take the returned execution_id and observe it in a later shell call. Use shellboy shell get --execution ID to check the actual command, cwd, Thread, Run and Turn before diagnosing unexpected output. CLI IDs require named flags, not positional arguments; consult the specific subcommand --help before guessing syntax. Guides are bundled with the executable: use shellboy docs list or shellboy docs search --query TEXT, then shellboy docs get --id ID to read the relevant guide before unfamiliar operations, especially delegation. No repository path is required. Use shellboy --help and shellboy history commands to inspect state, send messages, create children, wait, attach files and retrieve original history. Report actual results and errors. Commands share filesystem access; histories and cwd are per Thread."}]}),
    ];
    if let Some(body) = instruction_body {
        input.push(json!({"role":"developer","content":[{"type":"input_text","text":instruction_message(agents, body)?}]}));
    }
    Ok(input)
}
/// The context item, saved as a `context` history node (#183).
pub(crate) fn context_item(line: &ContextLine<'_>) -> Value {
    let effort = line.effort.map(str::to_owned);
    json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy context v1: thread={} run={} turn={} cwd={} connection={} model={} effort={:?}; AGENTS reference={}",line.thread,line.run,line.turn,line.cwd.display(),line.profile,line.model,effort,line.agents.display())}]})
}

/// Node kind of a saved context line.
const CONTEXT_NODE_KIND: &str = "context";

/// The context line last saved on the selected path, if any.
pub(crate) fn last_saved_context(
    db: &rusqlite::Connection,
    thread: &str,
) -> rusqlite::Result<Option<Value>> {
    let saved: Option<String> = db.query_row("SELECT n.payload_json FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind=?2 AND n.deleted_reason IS NULL AND n.payload_json IS NOT NULL ORDER BY p.ordinal DESC LIMIT 1",params![thread,CONTEXT_NODE_KIND],|r|r.get(0)).optional()?;
    Ok(saved.and_then(|payload| serde_json::from_str(&payload).ok()))
}

impl Snapshot {
    fn context(&self) -> Value {
        context_item(&ContextLine {
            thread: &self.thread,
            run: &self.run,
            turn: &self.turn,
            cwd: &self.cwd,
            profile: &self.profile,
            model: &self.model,
            effort: self.effort.as_deref(),
            agents: &self.agents,
        })
    }
    /// Keep the current context in the saved history (#183). It is appended
    /// once at the Turn start, again when a field changes (cwd, connection,
    /// model, effort), and again when a replacement (summary, spill, omission)
    /// removed it from the selected path. Each append is at the tail, so the
    /// requests of a Turn stay strict continuations of each other, and the
    /// latest saved context line is always the current one.
    pub(super) fn ensure_context_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        now: &str,
    ) -> Result<(), crate::storage::StorageError> {
        let item = self.context();
        if last_saved_context(tx, &self.thread)?.as_ref() == Some(&item) {
            return Ok(());
        }
        history::append_node(
            tx,
            &self.thread,
            &format!("context-{}", id()),
            Some(&self.turn),
            CONTEXT_NODE_KIND,
            &item,
            now,
        )
    }
}

pub(super) fn prepare_request(
    snapshot: &Snapshot,
    instruction_body: Option<&Arc<str>>,
    prepared: &mut crate::attachments::PreparedAttachments,
) -> Result<Value, ApiError> {
    let mut input = leading_items(&snapshot.agents, instruction_body.map(|body| &**body))?;
    // The context line is saved history (#183), so the replay already holds it.
    if let Some(replay) = &snapshot.input {
        crate::control::attachment_input::bind(replay, prepared, &mut input)?;
    }
    let mut request =
        model::response_request(&snapshot.model, input, snapshot.effort.as_deref(), true);
    model::cache_diagnostics::apply_request(
        &mut request,
        snapshot.cache_diagnostics_enabled,
        snapshot.cache_comparison_response_id.as_deref(),
    );
    Ok(request)
}
// Preserve the client's existing bounded partial-text/items/raw recovery type.
#[allow(clippy::result_large_err)]
pub(super) async fn stream_response(
    control: &Control,
    thread: &str,
    attempt: &str,
    client: &HttpModelClient,
    mut request: Value,
    bypass_usage_limit: Option<model::usage_limit::UsageLimit>,
    stop: &CancellationToken,
) -> Result<model::ModelResponse, model::http::AttemptFailure> {
    model::configure_shell_description(&mut request, control.store.config.shell.describe_calls);
    let (send, mut receive) = mpsc::channel::<model::http::ObservedEvent>(32);
    let mut stream_chars = 0usize;
    let mut last_receive_notice: Option<std::time::Instant> = None;
    let response = client.response_observed(
        thread,
        request,
        stop.clone(),
        Some(send),
        bypass_usage_limit,
    );
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
                    control.publish_event(EngineEvent::ModelReceive {
                        thread_id: thread.into(),
                        attempt_id: attempt.into(),
                        observed_at,
                    });
                }
                let Some(event) = event else { continue; };
                let kind=model::classify_stream_event(&event);
                let reasoning=kind==model::StreamEventKind::ReasoningDelta;
                if matches!(kind,model::StreamEventKind::TextDelta|model::StreamEventKind::ReasoningDelta) && let Some(delta)=event.get("delta").and_then(Value::as_str){let stream_char_offset=stream_chars;stream_chars+=delta.chars().count();control.publish_event(EngineEvent::Stream{thread_id:thread.into(),node_id:attempt.into(),text:delta.into(),reasoning,stream_char_offset});}
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
        input_characters: u64,
        history_characters: u64,
        extra_digest: Option<String>,
    ) -> Result<bool, crate::storage::StorageError> {
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id JOIN thread t ON t.id=u.thread_id WHERE u.id=?1 AND u.generation=?2 AND u.state IN ('preparing','running','waiting') AND u.interrupt_requested_at IS NULL AND r.state='live' AND t.selection_revision=?3 AND t.profile_revision=?4)",params![self.turn,self.generation,self.selection,self.profile_revision],|r|r.get(0))?;
        if !valid {
            return Ok(false);
        }
        if tx.execute("UPDATE turn SET state='running' WHERE id=?1 AND generation=?2 AND state IN ('preparing','running','waiting')",params![self.turn,self.generation])? != 1 {
            return Err(ApiError::new(ErrorCode::OperationConflict,"Turn changed before attempt admission").into());
        }
        let mut manifest = json!({"receipts":self.receipts,"events":self.events,"attachments":attempt_attachments.manifest,"input_characters":input_characters,"history_characters":history_characters});
        // Enough to rebuild this request later from saved state (#137): which
        // path nodes were replayed, and the cwd its context line named.
        if let Some(replay) = &self.input {
            manifest["replay"] = json!(replay.witness());
        }
        manifest["cwd"] = json!(self.cwd.to_string_lossy());
        manifest["prompt_cache"] = json!({
            "diagnostics_enabled":self.cache_diagnostics_enabled,
            "comparison_response_id":self.cache_comparison_response_id.as_deref().filter(|_|self.cache_diagnostics_enabled)
        });
        manifest["prompt_cache_diagnostics"] = Value::Null;
        // Only a hash: the contents stay in the user's configuration file.
        if let Some(digest) = extra_digest {
            manifest["extra_digest"] = json!(digest);
        }
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,effort,reasoning_epoch,state,input_manifest_json,started_at)VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'inflight',?9,?10)",params![attempt_id,self.turn,self.generation,self.selection,self.profile,self.model,self.effort,self.epoch,manifest.to_string(),now()])?;
        for event in &self.events {
            tx.execute("UPDATE delivery SET included_attempt_id=?3 WHERE event_id=?1 AND recipient_id=?2 AND state='pending'",params![event,self.run,attempt_id])?;
        }
        crate::attachments::adopt_prepared_tx(tx, attempt_attachments, attempt_id)
            .map_err(crate::storage::StorageError::from)?;
        Ok(true)
    }
}

#[cfg(test)]
mod instruction_header_tests {
    use super::*;

    #[test]
    fn instruction_header_is_one_safe_line_and_body_bytes_are_unchanged() {
        let source = "/synthetic/role\"\\\n\r\t\u{007f}\u{0085}\u{2028}\u{2029}.md";
        let body = "unchanged 日本語\r\n\nsecond line\u{0001}";
        let message = instruction_message(std::path::Path::new(source), body).unwrap();
        let (header, actual) = message.split_once('\n').unwrap();
        assert_eq!(actual.as_bytes(), body.as_bytes());
        assert!(
            !header
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
        );
        let quoted = header.strip_prefix("Working instructions from ").unwrap()
            .strip_suffix(" (user-editable; re-read at each new Turn start; same-Turn retries use the saved snapshot):").unwrap();
        assert_eq!(serde_json::from_str::<String>(quoted).unwrap(), source);
        for escaped in [
            "\\n", "\\r", "\\t", "\\u007f", "\\u0085", "\\u2028", "\\u2029",
        ] {
            assert!(header.contains(escaped), "{escaped}: {header}");
        }
    }

    #[test]
    fn present_empty_instructions_keep_an_empty_body_after_the_header() {
        let message = instruction_message(std::path::Path::new("/role.md"), "").unwrap();
        assert_eq!(
            message,
            "Working instructions from \"/role.md\" (user-editable; re-read at each new Turn start; same-Turn retries use the saved snapshot):\n"
        );
        assert_eq!(message.split_once('\n').unwrap().1, "");
    }
}
