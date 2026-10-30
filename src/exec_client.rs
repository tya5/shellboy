//! Noninteractive composition of existing owner RPCs, with local final-result validation.
use crate::{cli::ThreadCreateOptions, ipc, protocol::*};
use clap::Args;
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

#[derive(Debug, Clone, Args)]
pub struct ExecOptions {
    /// JSON Schema file (bounded, documented subset; unsupported keywords are rejected).
    #[arg(long)]
    pub schema: PathBuf,
    /// Existing conversation; otherwise create a new saved conversation.
    #[arg(long="thread",conflicts_with_all=["agents","cwd","connection","model","effort","parent_id"])]
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
    answer_node_id: Option<String>,
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
    fn final_answer_read(error: ApiError) -> Self {
        if matches!(
            error.code,
            ErrorCode::InvalidArgument | ErrorCode::ProtocolMismatch
        ) {
            return Self {code:6,status:"owner_version_mismatch",message:"Owner/CLI version mismatch: the required final_answer_only read was refused. Use matching owner and CLI versions. If work already ran, read the saved answer with history get --node <answer_node_id> without --final-answer-only; do not resend the work.".into(),api:Some(Box::new(error))};
        }
        if error.code == ErrorCode::IoFailed {
            return Self {code:6,status:"owner_capability_unavailable",message:"Cannot confirm the owner's final-answer read support; check matching owner/CLI versions. If work already ran, inspect the reported IDs with ordinary history get rather than resending it.".into(),api:Some(Box::new(error))};
        }
        Self::api(error)
    }
    fn api(error: ApiError) -> Self {
        let code = match error.code {
            ErrorCode::InvalidArgument
            | ErrorCode::ThreadNotFound
            | ErrorCode::InputLimit
            | ErrorCode::ConfigInvalid
            | ErrorCode::WaitCycle => 2,
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
                    limit: 1,
                    cursor: None,
                    query: None,
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
    async fn verify_final_answer_support(&mut self) -> Result<(), Failure> {
        // Missing-node rejection proves argument decoding without creating work.
        let probe: Result<HistoryBody, Failure> = self
            .request(
                "capability",
                Command::HistoryGet(HistoryGetArgs {
                    final_answer_only: true,
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
        let final_answer_read =
            matches!(&command,Command::HistoryGet(args)if args.final_answer_only);
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
            .map_err(|_|Failure::new(5,"timeout","Client deadline expired; inspect the reported IDs before resending; no target was stopped"))?.map_err(|error|if final_answer_read {Failure::final_answer_read(error)}else{Failure::api(error)})?;
        if !response.ok {
            let error = response.error.unwrap_or_else(|| {
                ApiError::new(ErrorCode::InvalidState, "Owner returned no error detail")
            });
            return Err(if final_answer_read {
                Failure::final_answer_read(error)
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
    async fn answer(&mut self) -> Result<String, Failure> {
        let turn = self.progress.turn_id.clone().ok_or_else(|| {
            Failure::new(
                4,
                "held",
                "Message has no assigned Turn; inspect its saved receipt",
            )
        })?;
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
                    self.progress.answer_node_id = facts.answer_node_id;
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
                _ => {}
            }
            let remaining = self
                .deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(30_000) as u64;
            if remaining == 0 {
                return Err(Failure::new(
                    5,
                    "timeout",
                    "Client deadline expired; no target was stopped",
                ));
            }
            let wait: WaitResult = self
                .request(
                    "wait",
                    Command::Wait(WaitArgs {
                        conditions: vec![WaitCondition::TurnCompleted {
                            turn_id: turn.clone(),
                        }],
                        timeout_ms: remaining,
                    }),
                )
                .await?;
            if wait.status == WaitStatus::RecoveryWait {
                return Err(Failure::new(
                    4,
                    "recovery_wait",
                    "Target requires explicit recovery; no automatic retry was requested",
                ));
            }
            if wait.status == WaitStatus::Unavailable {
                return Err(Failure::new(
                    4,
                    "execution_unavailable",
                    "Wait target became unavailable",
                ));
            }
            // Satisfied, interrupted and timeout are observations, not success evidence.
        }
        let node = self.progress.answer_node_id.clone().ok_or_else(|| {
            Failure::new(
                4,
                "execution_unavailable",
                "Completed Turn has no saved answer",
            )
        })?;
        let mut cursor = None;
        let mut text = String::new();
        for _ in 0..65 {
            let page: HistoryBody = self
                .request(
                    "answer",
                    Command::HistoryGet(HistoryGetArgs {
                        final_answer_only: true,
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
                    "Saved answer is no longer available",
                ));
            }
            text.push_str(&page.text);
            if text.len() > HISTORY_LIMIT {
                return Err(Failure::new(
                    6,
                    "output_limit",
                    "Saved answer exceeds the bounded client output size; inspect answer_node_id",
                ));
            }
            match page.next_cursor {
                None => {
                    if text.len() > ANSWER_LIMIT {
                        return Err(Failure::new(
                            6,
                            "output_limit",
                            "Final answer exceeds 1MiB; inspect answer_node_id",
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
            "Answer exceeded the bounded page count; inspect answer_node_id",
        ))
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
            || options.create.parent_id.is_some())
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
    let prompt = contract(&text, &schema);
    if prompt.len() > INPUT_LIMIT {
        return Err("Composed request and Schema exceed128KiB".into());
    }
    Command::MessageSend(MessageSendArgs {
        thread_id: Some("preflight".into()),
        text: prompt.clone(),
        now: false,
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
        parent_id: options.create.parent_id.clone(),
    };
    Command::ThreadCreate(create.clone())
        .validate()
        .map_err(|e| e.message)?;
    Ok((schema, prompt, create))
}
fn contract(text: &str, schema: &Schema) -> String {
    format!(
        "{text}\n\nFinal answer contract: perform the work normally with the available tools. When finished, return ONLY one JSON value matching this JSON Schema, with no Markdown fences or surrounding prose.\n{}",
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
        session.connect().await?;
        session.verify_final_answer_support().await?;
        if session.progress.thread_id.is_none() {
            let created:Value=session.request("create",Command::ThreadCreate(create)).await?;
            session.progress.thread_id=Some(created["thread_id"].as_str().ok_or_else(||Failure::new(6,"owner_error","Thread creation returned no ID"))?.to_owned());
        }
        for repair in 0..=options.max_repairs {
            session.progress.repairs=repair;session.progress.answer_node_id=None;session.progress.message_id=None;session.progress.run_id=None;session.progress.turn_id=None;
            let accepted:MessageAccepted=session.request("send",Command::MessageSend(MessageSendArgs{thread_id:session.progress.thread_id.clone(),text:prompt,now:false,sender_label:Some(options.sender_label.clone())})).await?;
            session.progress.message_id=Some(accepted.message_id);session.progress.run_id=accepted.run_id;session.progress.turn_id=accepted.turn_id;
            let raw=session.answer().await?;
            session.progress.stage="validate";
            let errors=schema.check_raw(&raw);
            if Instant::now()>=session.deadline {return Err(Failure::new(5,"timeout","Client deadline expired during validation; no target was stopped"));}
            if errors.is_empty() {return Ok(session.finish(Some(raw),0,"completed",json!({})));}
            if repair==options.max_repairs {return Ok(session.finish(None,3,"schema_mismatch",json!({"raw_answer":raw,"validation_errors":errors})));}
            prompt=contract(&format!("Correct ONLY the previous final answer; do not repeat completed side effects. Local validation failed: {}",serde_json::to_string(&errors).expect("errors JSON")),&schema);
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
                Failure::final_answer_read(ApiError::new(code, "synthetic final-read refusal"));
            assert_eq!(failure.code, 6);
            assert!(failure.message.contains("history get"));
            assert_eq!(failure.api.unwrap().code, code);
        }
        assert_eq!(
            Failure::api(ApiError::new(ErrorCode::InvalidArgument, "bad user input")).code,
            2
        );
        assert_eq!(
            Failure::final_answer_read(ApiError::new(ErrorCode::TargetUnavailable, "node is gone"))
                .code,
            4
        );
    }
}
