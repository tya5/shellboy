//! Noninteractive composition of existing owner RPCs, with local final-result validation.
use crate::{cli::ThreadCreateOptions, ipc, protocol::*};
use clap::{Args, ValueEnum};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tokio::{
    io::AsyncReadExt,
    time::{Duration, Instant, timeout_at},
};
mod schema;
use schema::Schema;
const INPUT_LIMIT: usize = 128 * 1024;
const SCHEMA_LIMIT: usize = 64 * 1024;
const HISTORY_LIMIT: usize = 4 * 1024 * 1024;
const ANSWER_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ResultMode {
    Reply,
    TurnEnd,
    RunEnd,
}

#[derive(Debug, Clone, Args)]
pub struct ExecOptions {
    /// Select the saved reply or end message to return.
    #[arg(long, value_enum, default_value = "reply")]
    pub result: ResultMode,
    /// JSON Schema file (bounded, documented subset; unsupported keywords are rejected).
    #[arg(long)]
    pub schema: PathBuf,
    /// Existing conversation; otherwise create a new saved conversation.
    #[arg(long="thread",conflicts_with_all=["agents","cwd","connection","model","effort","parent_thread_id"])]
    pub thread_id: Option<String>,
    #[command(flatten)]
    pub create: ThreadCreateOptions,
    #[arg(required_unless_present = "text_stdin", conflicts_with = "text_stdin")]
    pub text: Option<String>,
    #[arg(long)]
    pub text_stdin: bool,
    /// Corrective user messages after an invalid final answer; each may incur model/tool costs.
    #[arg(long,default_value_t=2,value_parser=clap::value_parser!(u32).range(0..=5))]
    pub max_repairs: u32,
    /// Overall RPC/repair deadline; expiry never stops the target.
    #[arg(long,default_value_t=300_000,value_parser=clap::value_parser!(u64).range(1..=86_400_000))]
    pub timeout_ms: u64,
    /// Unverified display name, never authority.
    #[arg(long, default_value = "shellboy exec")]
    pub sender_label: String,
}
#[derive(Default, Serialize)]
struct Progress {
    stage: &'static str,
    thread_id: Option<String>,
    message_id: Option<String>,
    run_id: Option<String>,
    turn_id: Option<String>,
    end_message_node_id: Option<String>,
    operation_id: Option<String>,
    send_operation_id: Option<String>,
    create_operation_id: Option<String>,
    repairs: u32,
    turn_usage: Vec<Value>,
}
pub struct Outcome {
    pub code: u8,
    pub result: Option<String>,
    pub diagnostics: Value,
}
impl Outcome {
    pub fn input(message: &str) -> Self {
        Self {
            code: 2,
            result: None,
            diagnostics: json!({"command":"exec","status":"input_error","message":message}),
        }
    }
}
struct Failure {
    code: u8,
    status: &'static str,
    message: String,
    api: Option<Box<ApiError>>,
}
impl Failure {
    fn new(code: u8, status: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            status,
            message: message.into(),
            api: None,
        }
    }
    fn end_message_read(error: ApiError) -> Self {
        if matches!(
            error.code,
            ErrorCode::InvalidArgument | ErrorCode::ProtocolMismatch
        ) {
            return Self {code:6,status:"owner_version_mismatch",message:"Owner/CLI version mismatch: the required end_message_only read was refused. Use matching owner and CLI versions. If work already ran, read the saved end message with history read --node <end_message_node_id> without --end-message-only; do not resend the work.".into(),api:Some(Box::new(error))};
        }
        if error.code == ErrorCode::IoFailed {
            return Self {code:6,status:"owner_capability_unavailable",message:"Cannot confirm the owner's end-message read support; check matching owner/CLI versions. If work already ran, inspect the reported IDs with ordinary history read rather than resending it.".into(),api:Some(Box::new(error))};
        }
        Self::api(error)
    }
    fn api(error: ApiError) -> Self {
        let code = match error.code {
            ErrorCode::InvalidArgument
            | ErrorCode::ThreadNotFound
            | ErrorCode::NotFound
            | ErrorCode::InputLimit
            | ErrorCode::ConfigInvalid
            | ErrorCode::SelfWait => 2,
            ErrorCode::InvalidState | ErrorCode::TargetUnavailable => 4,
            _ => 6,
        };
        Self {
            code,
            status: if code == 2 {
                "input_error"
            } else if code == 4 {
                "execution_unavailable"
            } else {
                "owner_error"
            },
            message: error.message.clone(),
            api: Some(Box::new(error)),
        }
    }
}
struct Session {
    data: PathBuf,
    origin: Option<Origin>,
    instance: String,
    deadline: Instant,
    progress: Progress,
}
impl Session {
    fn finish(&self, result: Option<String>, code: u8, status: &str, extra: Value) -> Outcome {
        let mut diagnostics = serde_json::to_value(&self.progress).expect("progress JSON");
        let fields = diagnostics.as_object_mut().expect("progress object");
        fields.insert("command".into(), json!("exec"));
        fields.insert("status".into(), json!(status));
        if let Some(extra) = extra.as_object() {
            fields.extend(extra.clone());
        }
        Outcome {
            code,
            result,
            diagnostics,
        }
    }
    async fn connect(&mut self) -> Result<(), Failure> {
        self.progress.stage = "connect";
        let response = timeout_at(
            self.deadline,
            ipc::call(
                &self.data,
                Command::ThreadList(ThreadListArgs {
                    thread_ids: None,
                    state: None,
                    order: Default::default(),
                    limit: 1,
                    cursor: None,
                    query: None,
                    awaiting: None,
                    archived: Default::default(),
                    tags: Vec::new(),
                    cwd: None,
                }),
                None,
                self.origin.clone(),
            ),
        )
        .await
        .map_err(|_| {
            Failure::new(
                5,
                "timeout",
                "Client execution deadline expired; no target was stopped",
            )
        })?
        .map_err(Failure::api)?;
        if !response.ok {
            return Err(Failure::api(response.error.unwrap_or_else(|| {
                ApiError::new(ErrorCode::InvalidState, "Owner rejected connection probe")
            })));
        }
        self.instance = response
            .operation_id
            .split_once(':')
            .map(|(id, _)| id.to_owned())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                Failure::new(
                    6,
                    "owner_error",
                    "Owner probe returned no instance-scoped operation ID",
                )
            })?;
        Ok(())
    }
    async fn verify_end_message_support(&mut self) -> Result<(), Failure> {
        // Missing-node rejection proves argument decoding without creating work.
        let probe: Result<HistoryBody, Failure> = self
            .request(
                "capability",
                Command::HistoryRead(HistoryReadArgs {
                    end_message_only: true,
                    parts: false,
                    node_id: uuid::Uuid::new_v4().to_string(),
                    limit_bytes: 1,
                    cursor: None,
                    encoding: "UTF-8".into(),
                }),
            )
            .await;
        match probe {
            Err(failure)
                if failure
                    .api
                    .as_ref()
                    .is_some_and(|error| error.code == ErrorCode::TargetUnavailable) =>
            {
                Ok(())
            }
            Ok(_) => Ok(()),
            Err(failure) => Err(failure),
        }
    }
    async fn request<T: DeserializeOwned>(
        &mut self,
        stage: &'static str,
        command: Command,
    ) -> Result<T, Failure> {
        let end_message_read =
            matches!(&command,Command::HistoryRead(args)if args.end_message_only);
        self.progress.stage = stage;
        if Instant::now() >= self.deadline {
            return Err(Failure::new(
                5,
                "timeout",
                "Client deadline expired; no target was stopped",
            ));
        }
        let operation = format!("{}:exec-{}", self.instance, uuid::Uuid::new_v4());
        self.progress.operation_id = Some(operation.clone());
        if stage == "send" {
            self.progress.send_operation_id = Some(operation.clone());
        }
        if stage == "create" {
            self.progress.create_operation_id = Some(operation.clone());
        }
        let response=timeout_at(self.deadline,ipc::call(&self.data,command,Some(operation),self.origin.clone())).await
            .map_err(|_|Failure::new(5,"timeout","Client deadline expired; inspect the reported IDs before resending; no target was stopped"))?.map_err(|error|if end_message_read {Failure::end_message_read(error)}else{Failure::api(error)})?;
        if !response.ok {
            let error = response.error.unwrap_or_else(|| {
                ApiError::new(ErrorCode::InvalidState, "Owner returned no error detail")
            });
            return Err(if end_message_read {
                Failure::end_message_read(error)
            } else {
                Failure::api(error)
            });
        }
        serde_json::from_value(
            response
                .result
                .ok_or_else(|| Failure::new(6, "owner_error", "Owner response has no result"))?,
        )
        .map_err(|_| Failure::new(6, "owner_error", "Owner returned an invalid typed result"))
    }
    /// One owner `wait` for the rest of the client deadline, shared by every
    /// exec wait (#441, #446): no slices, no re-arm. Callers classify its
    /// result with [`deadline_expired`] and [`interrupted`].
    async fn wait_once(&mut self, condition: WaitCondition) -> Result<WaitResult, Failure> {
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining == 0 {
            return Err(Failure::new(
                5,
                "timeout",
                "Client deadline expired; no target was stopped",
            ));
        }
        self.request(
            "wait",
            Command::Wait(WaitArgs {
                after: None,
                conditions: vec![condition],
                timeout_ms: remaining,
            }),
        )
        .await
    }
    /// `--result reply`: the saved duty is classified by the owner's open-state
    /// rule; only an open duty gets one `reply_received` wait for the rest of
    /// the client deadline, whose snapshot of the message decides (#441).
    async fn await_reply(
        &mut self,
        message: &str,
        saved: Option<&Value>,
    ) -> Result<Option<String>, Failure> {
        let saved = saved
            .ok_or_else(|| Failure::new(4, "reply_unavailable", "No reply outcome was saved"))?;
        let Some(state) = open_state(saved) else {
            return ended_reply(saved["state"].as_str(), saved["node_id"].as_str());
        };
        // The deadline can also expire before the owner answers; this path
        // owns that diagnostic, naming the saved state.
        let wait = self
            .wait_once(WaitCondition::ReplyReceived {
                message_id: message.to_owned(),
            })
            .await
            .map_err(|failure| failure.at_deadline(&open_duty(state)))?;
        // The owner's status decides; its snapshot of this exact message
        // carries the reply state and node.
        let reply = reply_snapshot(&wait.snapshot, message).unwrap_or(Value::Null);
        match (wait.status, open_state(&reply)) {
            (WaitStatus::Satisfied, None) if reply["state"].is_string() => {
                ended_reply(reply["state"].as_str(), reply["node_id"].as_str())
            }
            (WaitStatus::Timeout, Some(state)) => Err(deadline_expired(&open_duty(state))),
            (WaitStatus::Interrupted, Some(state)) => Err(interrupted(&open_duty(state))),
            _ => Err(impossible_reply_wait()),
        }
    }
    async fn answer(&mut self, mode: ResultMode) -> Result<String, Failure> {
        if self.progress.run_id.is_none() {
            return Err(Failure::new(4, "held", "Message has no assigned Run"));
        }
        if matches!(mode, ResultMode::RunEnd | ResultMode::Reply) {
            let run = self.progress.run_id.clone().expect("assigned run");
            let target = format!("Run {run} has not ended");
            let mut waited = false;
            loop {
                let facts: RunDetails = self
                    .request(
                        "run",
                        Command::RunGet(RunTarget {
                            run_id: run.clone(),
                        }),
                    )
                    .await?;
                match facts.state {
                    RunState::Completed => {
                        let message = self.progress.message_id.clone().expect("accepted message");
                        let delivery: MessageDetails = self
                            .request(
                                "message",
                                Command::MessageGet(MessageTarget {
                                    message_id: message.clone(),
                                }),
                            )
                            .await?;
                        if delivery.delivery_status != DeliveryStatus::Applied {
                            return Err(Failure::new(4, "held", "Message was not applied"));
                        }
                        self.progress.end_message_node_id = if mode == ResultMode::Reply {
                            self.await_reply(&message, delivery.reply.as_ref()).await?
                        } else {
                            facts.end_message_node_id
                        };
                        break;
                    }
                    RunState::Stopped | RunState::Failed if mode == ResultMode::Reply => {
                        // A Stop settles the duty unless it awaits the recipient's
                        // own ask (#433); the saved reply state decides (#441).
                        let message = self.progress.message_id.clone().expect("accepted message");
                        let delivery: MessageDetails = self
                            .request(
                                "message",
                                Command::MessageGet(MessageTarget {
                                    message_id: message.clone(),
                                }),
                            )
                            .await?;
                        if delivery.delivery_status != DeliveryStatus::Applied {
                            return Err(Failure::new(
                                4,
                                "execution_unavailable",
                                "Run ended without a successful end message",
                            ));
                        }
                        self.progress.end_message_node_id =
                            self.await_reply(&message, delivery.reply.as_ref()).await?;
                        break;
                    }
                    RunState::Stopped | RunState::Failed => {
                        return Err(Failure::new(
                            4,
                            "execution_unavailable",
                            "Run ended without a successful end message",
                        ));
                    }
                    // A satisfied wait means the Run ended; it is never re-armed.
                    _ if waited => return Err(unended_after_wait()),
                    _ => {}
                }
                let wait = self
                    .wait_once(WaitCondition::RunEnded {
                        run_id: run.clone(),
                    })
                    .await
                    .map_err(|failure| failure.at_deadline(&target))?;
                match wait.status {
                    WaitStatus::Satisfied => waited = true,
                    WaitStatus::Timeout => return Err(deadline_expired(&target)),
                    WaitStatus::Interrupted => return Err(interrupted(&target)),
                    WaitStatus::RecoveryWait => {
                        return Err(Failure::new(
                            4,
                            "recovery_wait",
                            "Target requires explicit recovery",
                        ));
                    }
                    WaitStatus::Unavailable => return Err(wait_unavailable()),
                }
            }
        } else {
            let turn = self.progress.turn_id.clone().ok_or_else(|| {
                Failure::new(
                    4,
                    "held",
                    "Message has no assigned Turn; inspect its saved receipt",
                )
            })?;
            let target = format!("Turn {turn} has not completed");
            let mut waited = false;
            loop {
                let facts: TurnDetails = self
                    .request(
                        "turn",
                        Command::TurnGet(TurnTarget {
                            turn_id: turn.clone(),
                        }),
                    )
                    .await?;
                match facts.state {
                    TurnState::Completed => {
                        self.progress
                            .turn_usage
                            .push(json!({"turn_id":turn,"usage_totals":facts.usage_totals}));
                        let message = self.progress.message_id.clone().expect("accepted message");
                        let delivery: MessageDetails = self
                            .request(
                                "message",
                                Command::MessageGet(MessageTarget {
                                    message_id: message,
                                }),
                            )
                            .await?;
                        if delivery.delivery_status != DeliveryStatus::Applied {
                            return Err(Failure::new(
                                4,
                                "held",
                                "Message was not applied; the completed Turn is not proof of its answer",
                            ));
                        }
                        self.progress.end_message_node_id = facts.end_message_node_id;
                        break;
                    }
                    TurnState::RecoveryWait => {
                        return Err(Failure::new(
                            4,
                            "recovery_wait",
                            "Target requires explicit recovery; no automatic retry was requested",
                        ));
                    }
                    TurnState::Stopped | TurnState::Failed => {
                        return Err(Failure::new(
                            4,
                            "execution_unavailable",
                            "Target ended without a successful answer",
                        ));
                    }
                    // A satisfied wait means the Turn ended; it is never re-armed.
                    _ if waited => return Err(unended_after_wait()),
                    _ => {}
                }
                let wait = self
                    .wait_once(WaitCondition::TurnCompleted {
                        turn_id: turn.clone(),
                    })
                    .await
                    .map_err(|failure| failure.at_deadline(&target))?;
                // Satisfied is an observation, not success evidence: the
                // saved Turn is read again.
                match wait.status {
                    WaitStatus::Satisfied => waited = true,
                    WaitStatus::Timeout => return Err(deadline_expired(&target)),
                    WaitStatus::Interrupted => return Err(interrupted(&target)),
                    WaitStatus::RecoveryWait => {
                        return Err(Failure::new(
                            4,
                            "recovery_wait",
                            "Target requires explicit recovery; no automatic retry was requested",
                        ));
                    }
                    WaitStatus::Unavailable => return Err(wait_unavailable()),
                }
            }
        }
        let node = self.progress.end_message_node_id.clone().ok_or_else(|| {
            Failure::new(
                4,
                "execution_unavailable",
                "Completed Turn has no saved end message",
            )
        })?;
        let mut cursor = None;
        let mut text = String::new();
        for _ in 0..65 {
            let page: HistoryBody = self
                .request(
                    "answer",
                    Command::HistoryRead(HistoryReadArgs {
                        end_message_only: mode != ResultMode::Reply,
                        parts: false,
                        node_id: node.clone(),
                        limit_bytes: 65_536,
                        cursor: cursor.clone(),
                        encoding: "UTF-8".into(),
                    }),
                )
                .await?;
            if page.node_id != node {
                return Err(Failure::new(
                    6,
                    "owner_error",
                    "Answer read returned another node",
                ));
            }
            if page.tombstone_reason.is_some() {
                return Err(Failure::new(
                    4,
                    "execution_unavailable",
                    "Saved end message is no longer available",
                ));
            }
            text.push_str(&page.text);
            if text.len() > HISTORY_LIMIT {
                return Err(Failure::new(
                    6,
                    "output_limit",
                    "Saved end message exceeds the bounded client output size; inspect end_message_node_id",
                ));
            }
            match page.next_cursor {
                None => {
                    if text.len() > ANSWER_LIMIT {
                        return Err(Failure::new(
                            6,
                            "output_limit",
                            "End message exceeds 1MiB; inspect end_message_node_id",
                        ));
                    }
                    return Ok(text);
                }
                Some(next) if cursor.as_ref() != Some(&next) => cursor = Some(next),
                _ => {
                    return Err(Failure::new(
                        6,
                        "owner_error",
                        "Answer cursor did not advance",
                    ));
                }
            }
        }
        Err(Failure::new(
            6,
            "output_limit",
            "Answer exceeded the bounded page count; inspect end_message_node_id",
        ))
    }
}
/// The client deadline ended exec's observation while `target` is still open.
fn deadline_expired(target: &str) -> Failure {
    Failure::new(
        5,
        "timeout",
        format!("Client deadline expired while {target}; no target was stopped"),
    )
}
/// Input for the calling Thread ended exec's observation (the owner's
/// `interrupted`) while `target` is still open; it keeps running.
fn interrupted(target: &str) -> Failure {
    Failure::new(
        5,
        "interrupted",
        format!(
            "Wait was interrupted by input for the calling Thread while {target}; no target was stopped"
        ),
    )
}
fn open_duty(state: &str) -> String {
    format!("the reply duty is {state}")
}
fn wait_unavailable() -> Failure {
    Failure::new(4, "execution_unavailable", "Wait target became unavailable")
}
/// A saved target still open after the owner reported its wait satisfied.
fn unended_after_wait() -> Failure {
    Failure::new(
        6,
        "owner_error",
        "Owner reported the wait satisfied but the target has not ended",
    )
}
impl Failure {
    /// A client-deadline failure during a wait names the target still open.
    fn at_deadline(self, target: &str) -> Self {
        if self.code == 5 && self.status == "timeout" {
            deadline_expired(target)
        } else {
            self
        }
    }
}
/// The `reply_received` snapshot for one message, in the saved reply shape.
fn reply_snapshot(snapshot: &Value, message: &str) -> Option<Value> {
    snapshot["conditions"]
        .as_array()?
        .iter()
        .find(|item| item["type"] == "reply" && item["message_id"] == message)
        .map(|item| json!({"state": item["state"], "node_id": item["reply_node_id"]}))
}
/// The duty's state while the owner's single open-state rule holds it open.
fn open_state(reply: &Value) -> Option<&str> {
    reply["state"]
        .as_str()
        .filter(|state| crate::control::reply::is_open(Some(state)))
}
/// The result of a duty that is no longer open.
fn ended_reply(state: Option<&str>, node: Option<&str>) -> Result<Option<String>, Failure> {
    if state == Some("replied") {
        return Ok(node.map(str::to_owned));
    }
    Err(Failure::new(
        4,
        "reply_unavailable",
        format!(
            "Reply ended with state {} without a reply",
            state.unwrap_or("missing")
        ),
    ))
}
/// A reply wait result the owner never produces for a single reply condition.
fn impossible_reply_wait() -> Failure {
    Failure::new(
        6,
        "owner_error",
        "Owner returned an impossible result for the reply wait",
    )
}

#[cfg(test)]
mod reply_mode_tests {
    use super::*;
    #[test]
    fn terminal_nonreply_outcomes_are_exit_four() {
        for state in ["ignored", "cancelled", "recipient_deleted"] {
            let Err(error) = ended_reply(Some(state), None) else {
                panic!("{state} must fail");
            };
            assert_eq!(error.code, 4, "{state}");
            assert!(error.message.contains(state), "{}", error.message);
        }
        assert!(
            matches!(ended_reply(Some("replied"), Some("reply")), Ok(Some(node)) if node == "reply")
        );
    }

    #[test]
    fn wait_snapshot_supplies_the_reply_state_and_node() {
        let snapshot = json!({"conditions":[{"type":"reply","message_id":"other","state":"replied","reply_node_id":"x"},{"type":"reply","message_id":"m","state":"replied","reply_node_id":"node"}]});
        assert_eq!(
            reply_snapshot(&snapshot, "m"),
            Some(json!({"state":"replied","node_id":"node"}))
        );
        assert_eq!(reply_snapshot(&Value::Null, "m"), None);
    }

    #[test]
    fn deadline_inside_the_reply_wait_names_the_open_state() {
        let failure =
            Failure::new(5, "timeout", "Client deadline expired").at_deadline("awaiting_own_ask");
        assert_eq!((failure.code, failure.status), (5, "timeout"));
        assert!(
            failure.message.contains("awaiting_own_ask"),
            "{}",
            failure.message
        );
        assert!(failure.message.contains("no target was stopped"));
        let other = Failure::new(6, "owner_error", "lost").at_deadline("pending");
        assert_eq!((other.code, other.message.as_str()), (6, "lost"));
    }
}

async fn prepare(options: &ExecOptions) -> Result<(Schema, String, ThreadCreateArgs), String> {
    if options.max_repairs > 5 || !(1..=86_400_000).contains(&options.timeout_ms) {
        return Err("exec repair/timeout limits are invalid".into());
    }
    if options.text_stdin && options.text.is_some() {
        return Err("Choose text or --text-stdin, not both".into());
    }
    if options.thread_id.is_some()
        && (options.create.agents.is_some()
            || options.create.cwd.is_some()
            || options.create.connection.is_some()
            || options.create.model.is_some()
            || options.create.effort.is_some()
            || options.create.parent_thread_id.is_some())
    {
        return Err("Existing --thread conflicts with new-Thread options".into());
    }
    let metadata = tokio::fs::metadata(&options.schema)
        .await
        .map_err(|_| "Cannot read Schema file")?;
    if !metadata.is_file() || metadata.len() > SCHEMA_LIMIT as u64 {
        return Err("Schema must be a regular file of at most64KiB".into());
    }
    let mut bytes = Vec::new();
    tokio::fs::File::open(&options.schema)
        .await
        .map_err(|_| "Cannot open Schema file")?
        .take(SCHEMA_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "Cannot read Schema file")?;
    if bytes.len() > SCHEMA_LIMIT {
        return Err("Schema exceeds64KiB".into());
    }
    let schema = Schema::parse(&bytes)?;
    let text = if options.text_stdin {
        let mut bytes = Vec::new();
        tokio::io::stdin()
            .take(INPUT_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "Cannot read stdin")?;
        if bytes.len() > INPUT_LIMIT {
            return Err("stdin exceeds128KiB".into());
        }
        String::from_utf8(bytes).map_err(|_| "stdin must be UTF-8")?
    } else {
        options.text.clone().unwrap_or_default()
    };
    if text.trim().is_empty() {
        return Err("A nonempty work request is required".into());
    }
    let prompt = contract(&text, &schema, options.result);
    if prompt.len() > INPUT_LIMIT {
        return Err("Composed request and Schema exceed128KiB".into());
    }
    Command::MessageSend(MessageSendArgs {
        max_model_requests: None,
        human: false,
        resume_recovery: false,
        thread_id: Some("preflight".into()),
        text: prompt.clone(),
        deliver: Deliver::Next,
        sender_label: Some(options.sender_label.clone()),
    })
    .validate()
    .map_err(|e| e.message)?;
    fn path(path: Option<&Path>) -> Result<Option<String>, String> {
        path.map(|path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| "Paths must be UTF-8".into())
        })
        .transpose()
    }
    let create = ThreadCreateArgs {
        agents: path(options.create.agents.as_deref())?,
        cwd: path(options.create.cwd.as_deref())?,
        connection: options.create.connection.clone(),
        model: options.create.model.clone(),
        effort: options.create.effort.clone(),
        parent_thread_id: options.create.parent_thread_id.clone(),
    };
    Command::ThreadCreate(create.clone())
        .validate()
        .map_err(|e| e.message)?;
    Ok((schema, prompt, create))
}
fn contract(text: &str, schema: &Schema, mode: ResultMode) -> String {
    let instruction = if mode == ResultMode::Reply {
        "send a reply to this message containing ONLY"
    } else {
        "return ONLY"
    };
    format!(
        "{text}\n\nFinal answer contract: perform the work normally with the available tools. When finished, {instruction} one JSON value matching this JSON Schema, with no Markdown fences or surrounding prose.\n{}",
        schema.value()
    )
}
/// This client never starts an owner and never issues run stop/retry.
pub async fn execute(
    options: ExecOptions,
    data: &Path,
    cwd: &Path,
    origin: Option<Origin>,
    operation: Option<String>,
) -> Outcome {
    if operation.is_some() {
        return Outcome::input("exec performs multiple RPCs; --operation-id is not accepted");
    }
    let (schema, mut prompt, mut create) = match prepare(&options).await {
        Ok(v) => v,
        Err(e) => return Outcome::input(&e),
    };
    if create.cwd.is_none() {
        create.cwd = match cwd.to_str() {
            Some(path) => Some(path.into()),
            None => return Outcome::input("cwd must be UTF-8"),
        };
    }
    let mut session = Session {
        data: data.into(),
        origin,
        instance: String::new(),
        deadline: Instant::now() + Duration::from_millis(options.timeout_ms),
        progress: Progress {
            thread_id: options.thread_id.clone(),
            ..Default::default()
        },
    };
    let result: Result<Outcome,Failure>=async {
        session.progress.stage = "resolve_ids";
        timeout_at(session.deadline, crate::cli::resolve_conversation_ids(
            &session.data, session.origin.as_ref(), &mut session.progress.thread_id, &mut create.parent_thread_id,
        )).await.map_err(|_| Failure::new(5, "timeout", "Client execution deadline expired; no target was stopped"))?.map_err(Failure::api)?;
        session.connect().await?;
        session.verify_end_message_support().await?;
        if session.progress.thread_id.is_none() {
            let created:Value=session.request("create",Command::ThreadCreate(create)).await?;
            session.progress.thread_id=Some(created["thread_id"].as_str().ok_or_else(||Failure::new(6,"owner_error","Thread creation returned no ID"))?.to_owned());
        }
        for repair in 0..=options.max_repairs {
            session.progress.repairs=repair;session.progress.end_message_node_id=None;session.progress.message_id=None;session.progress.run_id=None;session.progress.turn_id=None;
            let thread_id = session.progress.thread_id.clone();
            let sender_label = Some(options.sender_label.clone());
            let command = if options.result == ResultMode::Reply {
                Command::MessageAsk(MessageAskArgs { human:false, sender_label, thread_id, text:prompt, to:None, deliver:Deliver::Next })
            } else {
                Command::MessageSend(MessageSendArgs { max_model_requests: None, human:false, sender_label, thread_id, text:prompt, resume_recovery:false, deliver:Deliver::Next })
            };
            let accepted:MessageAccepted=session.request("send",command).await?;
            session.progress.message_id=Some(accepted.message_id);session.progress.run_id=accepted.run_id;session.progress.turn_id=accepted.turn_id;
            let raw=session.answer(options.result).await?;
            session.progress.stage="validate";
            let errors=if raw.is_empty() { vec![] } else { schema.check_raw(&raw) };
            if Instant::now()>=session.deadline {return Err(Failure::new(5,"timeout","Client deadline expired during validation; no target was stopped"));}
            if errors.is_empty() {return Ok(session.finish(Some(raw),0,"completed",json!({})));}
            if repair==options.max_repairs {return Ok(session.finish(None,3,"schema_mismatch",json!({"raw_answer":raw,"validation_errors":errors})));}
            prompt=contract(&format!("Correct ONLY the previous final answer; do not repeat completed side effects. Local validation failed: {}",serde_json::to_string(&errors).expect("errors JSON")),&schema,options.result);
        }
        unreachable!("bounded repair loop returns")
    }.await;
    match result {
        Ok(outcome) => outcome,
        Err(failure) => session.finish(
            None,
            failure.code,
            failure.status,
            json!({"message":failure.message,"error":failure.api}),
        ),
    }
}

#[cfg(test)]
mod owner_version_tests {
    use super::*;
    #[test]
    fn projection_refusal_and_wire_close_are_owner_errors_without_masking_input_errors() {
        for code in [
            ErrorCode::InvalidArgument,
            ErrorCode::ProtocolMismatch,
            ErrorCode::IoFailed,
        ] {
            let failure =
                Failure::end_message_read(ApiError::new(code, "synthetic final-read refusal"));
            assert_eq!(failure.code, 6);
            assert!(failure.message.contains("history read"));
            assert_eq!(failure.api.unwrap().code, code);
        }
        assert_eq!(
            Failure::api(ApiError::new(ErrorCode::InvalidArgument, "bad user input")).code,
            2
        );
        assert_eq!(
            Failure::end_message_read(ApiError::new(ErrorCode::TargetUnavailable, "node is gone"))
                .code,
            4
        );
    }
}
