//! Interactive line client composed only from existing owner RPCs.
//!
//! It creates or selects a Thread, sends each input through `message.send`,
//! then polls the assigned Turn, its shell executions and the saved end message.
//! It owns no queue, table, worker or state beyond this process's loop.
use crate::{cli::ThreadCreateOptions, exec_client::ResultMode, ipc, protocol::*};
use clap::Args;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt},
    sync::mpsc,
    time::{Duration, Instant},
};

const ANSWER_LIMIT: usize = 1024 * 1024;
const SUMMARY_CHARS: usize = 120;

#[derive(Debug, Clone, Args)]
pub struct ChatOptions {
    /// Continue an existing conversation; otherwise create one on the first input.
    #[arg(long="thread",conflicts_with_all=["agents","cwd","connection","model","effort","parent_id"])]
    pub thread_id: Option<String>,
    #[command(flatten)]
    pub create: ThreadCreateOptions,
    /// Emit one JSON object per line on stdout for every event instead of text.
    #[arg(long)]
    pub json: bool,
    /// End of each message's wait: the Turn's end message, the Run's end message, or a reply.
    #[arg(long, value_enum, default_value = "turn-end")]
    pub until: ResultMode,
    /// Per-message wait deadline; expiry never stops the work. Default: no deadline.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=86_400_000))]
    pub timeout_ms: Option<u64>,
}

/// Process facts injected so tests need neither a terminal nor signals.
pub struct ChatEnv {
    /// Whether stdin is a terminal; true records `sender.kind = human`.
    pub stdin_is_tty: bool,
    /// One item per Ctrl+C.
    pub interrupts: mpsc::Receiver<()>,
}

#[derive(Debug)]
pub struct ChatError {
    pub code: u8,
    pub message: String,
}
impl ChatError {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn api(error: ApiError) -> Self {
        if error.code == ErrorCode::AppNotRunning {
            return Self::new(
                6,
                "No Shellboy owner is running for this data directory. Start one with `shellboy serve --data-dir DIR` (or open the TUI), then run chat again.",
            );
        }
        Self::new(6, error.message)
    }
}

/// Stable JSON line; absent facts are null, never omitted.
#[derive(Default, Serialize)]
struct Event {
    #[serde(rename = "type")]
    kind: &'static str,
    thread_id: Option<String>,
    run_id: Option<String>,
    turn_id: Option<String>,
    message_id: Option<String>,
    execution_id: Option<String>,
    node_id: Option<String>,
    state: Option<String>,
    summary: Option<String>,
    exit_code: Option<i64>,
    at: Option<String>,
    text: Option<String>,
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}
fn short(text: &str) -> String {
    let one: String = clean(text).split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > SUMMARY_CHARS {
        let cut: String = one.chars().take(SUMMARY_CHARS).collect();
        format!("{cut}...")
    } else {
        one
    }
}
fn text_of(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

struct Chat<'a, W: Write> {
    until: ResultMode,
    timeout: Option<Duration>,
    data: PathBuf,
    origin: Option<Origin>,
    instance: String,
    human: bool,
    json: bool,
    out: &'a mut W,
    thread: Option<String>,
    run: Option<String>,
    turn: Option<String>,
    message: Option<String>,
}

enum Flow {
    /// The message ended: 0 normally, 4 abnormal (stopped/failed/recovery_wait/held), 5 timeout.
    Ended(u8),
    Exit,
}

impl<W: Write> Chat<'_, W> {
    fn emit(&mut self, mut event: Event, human: impl Into<String>) {
        if self.json {
            event.thread_id = event.thread_id.or_else(|| self.thread.clone());
            event.run_id = event.run_id.or_else(|| self.run.clone());
            event.turn_id = event.turn_id.or_else(|| self.turn.clone());
            event.message_id = event.message_id.or_else(|| self.message.clone());
            let line = serde_json::to_string(&event).expect("event JSON");
            let _ = writeln!(self.out, "{line}");
        } else {
            let human = human.into();
            if !human.is_empty() {
                let _ = writeln!(self.out, "{human}");
            }
        }
        let _ = self.out.flush();
    }
    async fn call<T: DeserializeOwned>(&self, command: Command) -> Result<T, ChatError> {
        let operation = format!("{}:chat-{}", self.instance, uuid::Uuid::new_v4());
        let response = ipc::call(&self.data, command, Some(operation), self.origin.clone())
            .await
            .map_err(ChatError::api)?;
        if !response.ok {
            return Err(ChatError::api(response.error.unwrap_or_else(|| {
                ApiError::new(ErrorCode::InvalidState, "Owner returned no error detail")
            })));
        }
        serde_json::from_value(
            response
                .result
                .ok_or_else(|| ChatError::new(6, "Owner response has no result"))?,
        )
        .map_err(|_| ChatError::new(6, "Owner returned an invalid typed result"))
    }
    async fn connect(&mut self) -> Result<(), ChatError> {
        let response = ipc::call(
            &self.data,
            Command::ThreadList(ThreadListArgs {
                limit: 1,
                cursor: None,
                query: None,
            }),
            None,
            self.origin.clone(),
        )
        .await
        .map_err(ChatError::api)?;
        if !response.ok {
            return Err(ChatError::api(response.error.unwrap_or_else(|| {
                ApiError::new(ErrorCode::InvalidState, "Owner rejected connection probe")
            })));
        }
        self.instance = response
            .operation_id
            .split_once(':')
            .map(|(id, _)| id.to_owned())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ChatError::new(6, "Owner probe returned no instance-scoped ID"))?;
        Ok(())
    }

    /// One input: send, then follow the assigned Turn to its end.
    async fn exchange(
        &mut self,
        text: String,
        create: &ThreadCreateArgs,
        interrupts: &mut mpsc::Receiver<()>,
    ) -> Result<Flow, ChatError> {
        if self.thread.is_none() {
            let created: Value = self.call(Command::ThreadCreate(create.clone())).await?;
            let id = created["thread_id"]
                .as_str()
                .ok_or_else(|| ChatError::new(6, "Thread creation returned no ID"))?
                .to_owned();
            self.thread = Some(id.clone());
            self.emit(
                Event {
                    kind: "thread",
                    state: Some("created".into()),
                    ..Default::default()
                },
                format!("[thread] {id}"),
            );
        }
        let accepted: MessageAccepted = self
            .call(Command::MessageSend(MessageSendArgs {
                human: self.human,
                expect_reply: self.until == ResultMode::Reply,
                sender_label: None,
                thread_id: self.thread.clone(),
                text,
                deliver: Deliver::Next,
            }))
            .await?;
        self.message = Some(accepted.message_id.clone());
        self.run = accepted.run_id.clone();
        self.turn = accepted.turn_id.clone();
        self.emit(
            Event {
                kind: "message_accepted",
                state: Some(name(&accepted.delivery_status)),
                summary: Some(name(&accepted.assignment)),
                ..Default::default()
            },
            "",
        );
        let Some(turn) = accepted.turn_id.clone() else {
            self.emit(
                Event {
                    kind: "notice",
                    state: Some(name(&accepted.delivery_status)),
                    summary: Some("no Turn assigned yet".into()),
                    ..Default::default()
                },
                format!(
                    "[notice] message {} is {} with no Turn assigned yet; inspect it with `shellboy message get --message {}`",
                    accepted.message_id,
                    name(&accepted.delivery_status),
                    accepted.message_id
                ),
            );
            return Ok(Flow::Ended(4));
        };
        self.follow(&turn, interrupts).await
    }

    async fn follow(
        &mut self,
        turn: &str,
        interrupts: &mut mpsc::Receiver<()>,
    ) -> Result<Flow, ChatError> {
        let deadline = self.timeout.map(|t| Instant::now() + t);
        let mut interrupted = 0u32;
        let mut last_state = None;
        let mut tools: HashMap<String, bool> = HashMap::new();
        loop {
            let facts: TurnDetails = self
                .call(Command::TurnGet(TurnTarget {
                    turn_id: turn.into(),
                }))
                .await?;
            self.tool_progress(turn, &mut tools).await?;
            if last_state != Some(facts.state) {
                last_state = Some(facts.state);
                self.emit(
                    Event {
                        kind: "turn_state",
                        state: Some(name(&facts.state)),
                        ..Default::default()
                    },
                    format!("[turn] {}", name(&facts.state)),
                );
            }
            if facts.state == TurnState::RecoveryWait {
                let hint = facts.recovery_hint.clone().unwrap_or_default();
                self.emit(
                    Event {
                        kind: "notice",
                        state: Some("recovery_wait".into()),
                        summary: Some(short(&hint)),
                        ..Default::default()
                    },
                    format!(
                        "[notice] the Turn needs explicit recovery (shellboy run retry --run RUN_ID); not retried automatically. {}",
                        short(&hint)
                    ),
                );
                return Ok(Flow::Ended(4));
            }
            if self.until == ResultMode::TurnEnd {
                match facts.state {
                    TurnState::Completed => {
                        return self.end_message(facts.end_message_node_id).await;
                    }
                    TurnState::Stopped | TurnState::Failed => {
                        return Ok(self.abnormal(&name(&facts.state)));
                    }
                    _ => {}
                }
            } else {
                let run = self
                    .run
                    .clone()
                    .ok_or_else(|| ChatError::new(6, "no Run"))?;
                let run_facts: RunDetails = self
                    .call(Command::RunGet(RunTarget { run_id: run }))
                    .await?;
                match run_facts.state {
                    RunState::Completed if self.until == ResultMode::RunEnd => {
                        return self.end_message(run_facts.end_message_node_id).await;
                    }
                    RunState::Completed => return self.reply_message().await,
                    RunState::Stopped | RunState::Failed => {
                        let state = if run_facts.state == RunState::Stopped {
                            "stopped"
                        } else {
                            "failed"
                        };
                        return Ok(self.abnormal(state));
                    }
                    _ => {}
                }
            }
            let remaining = match deadline {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        self.emit(
                            Event {
                                kind: "notice",
                                state: Some("timeout".into()),
                                summary: Some("deadline expired; the work was not stopped".into()),
                                ..Default::default()
                            },
                            "[notice] timed out waiting; the work was not stopped",
                        );
                        return Ok(Flow::Ended(5));
                    }
                    left.as_millis().clamp(1, 1_000) as u64
                }
                None => 1_000,
            };
            let condition = if self.until == ResultMode::TurnEnd {
                WaitCondition::TurnCompleted {
                    turn_id: turn.into(),
                }
            } else {
                WaitCondition::RunEnded {
                    run_id: self.run.clone().unwrap_or_default(),
                }
            };
            let wait = self.call::<WaitResult>(Command::Wait(WaitArgs {
                after: None,
                conditions: vec![condition],
                timeout_ms: remaining,
            }));
            tokio::select! {
                biased;
                signal = interrupts.recv() => {
                    if signal.is_none() { continue; }
                    interrupted += 1;
                    if interrupted >= 2 {
                        self.emit(Event{kind:"notice",state:Some("detached".into()),summary:Some("chat exited; the Run is not touched further".into()),..Default::default()},
                            "[notice] leaving chat; the Run is not touched further");
                        return Ok(Flow::Exit);
                    }
                    if let Some(run) = self.run.clone() {
                        let _: Value = self.call(Command::RunStop(RunTarget { run_id: run })).await?;
                        self.emit(Event{kind:"run_stopping",state:Some("stopping".into()),..Default::default()},
                            "[stop] stopping the Run; press Ctrl+C again to leave chat");
                    }
                }
                result = wait => { result?; }
            }
        }
    }

    fn abnormal(&mut self, state: &str) -> Flow {
        self.emit(
            Event {
                kind: "notice",
                state: Some(state.into()),
                summary: Some("ended without an end message".into()),
                ..Default::default()
            },
            format!("[notice] ended as {state} without an end message"),
        );
        Flow::Ended(4)
    }

    async fn reply_message(&mut self) -> Result<Flow, ChatError> {
        let message = self.message.clone().unwrap_or_default();
        let details: MessageDetails = self
            .call(Command::MessageGet(MessageTarget {
                message_id: message,
            }))
            .await?;
        let reply = details.reply.unwrap_or(Value::Null);
        match (reply["state"].as_str(), reply["node_id"].as_str()) {
            (Some("replied"), Some(node)) => self.end_message(Some(node.to_owned())).await,
            (state, _) => Ok(self.abnormal(&format!("reply_{}", state.unwrap_or("missing")))),
        }
    }

    async fn tool_progress(
        &mut self,
        turn: &str,
        seen: &mut HashMap<String, bool>,
    ) -> Result<(), ChatError> {
        let page: Value = self
            .call(Command::ShellList(ShellListArgs {
                thread_id: self.thread.clone(),
                since: None,
                until: None,
                command_contains: None,
                state: None,
                exit_code: None,
                limit: 50,
                cursor: None,
            }))
            .await?;
        let items = page["items"].as_array().cloned().unwrap_or_default();
        for item in items.iter().rev() {
            if item["turn_id"].as_str() != Some(turn) {
                continue;
            }
            let Some(id) = item["execution_id"].as_str() else {
                continue;
            };
            let summary = text_of(&item["description"])
                .filter(|d| !d.is_empty())
                .or_else(|| text_of(&item["command_excerpt"]))
                .map(|s| short(&s))
                .unwrap_or_default();
            let state = item["state"].as_str().unwrap_or("running").to_owned();
            let finished = !matches!(state.as_str(), "starting" | "running" | "stopping");
            if !seen.contains_key(id) {
                seen.insert(id.to_owned(), false);
                self.emit(
                    Event {
                        kind: "tool_started",
                        execution_id: Some(id.into()),
                        state: Some("running".into()),
                        summary: Some(summary.clone()),
                        at: text_of(&item["started_at"]),
                        ..Default::default()
                    },
                    format!("[tool] {summary}"),
                );
            }
            if finished && seen.get(id) == Some(&false) {
                seen.insert(id.to_owned(), true);
                let code = item["exit_code"].as_i64();
                self.emit(
                    Event {
                        kind: "tool_finished",
                        execution_id: Some(id.into()),
                        state: Some(state.clone()),
                        summary: Some(summary),
                        exit_code: code,
                        at: text_of(&item["ended_at"]),
                        ..Default::default()
                    },
                    match code {
                        Some(code) => format!("[tool] {state}, exit {code}"),
                        None => format!("[tool] {state}"),
                    },
                );
            }
        }
        Ok(())
    }

    async fn end_message(&mut self, node: Option<String>) -> Result<Flow, ChatError> {
        let node =
            node.ok_or_else(|| ChatError::new(6, "Completed Turn has no saved end message"))?;
        let mut cursor = None;
        let mut text = String::new();
        for _ in 0..65 {
            let page: HistoryBody = self
                .call(Command::HistoryGet(HistoryGetArgs {
                    end_message_only: self.until != ResultMode::Reply,
                    parts: false,
                    node_id: node.clone(),
                    limit_bytes: 65_536,
                    cursor: cursor.clone(),
                    encoding: "UTF-8".into(),
                }))
                .await?;
            if page.tombstone_reason.is_some() {
                return Err(ChatError::new(
                    4,
                    "Saved end message is no longer available",
                ));
            }
            text.push_str(&page.text);
            if text.len() > ANSWER_LIMIT {
                return Err(ChatError::new(
                    6,
                    format!("End message exceeds 1MiB; read it with history get --node {node}"),
                ));
            }
            match page.next_cursor {
                None => {
                    let shown = format!("\n{}\n", clean(&text).trim_end());
                    self.emit(
                        Event {
                            kind: if self.until == ResultMode::Reply {
                                "reply"
                            } else {
                                "end_message"
                            },
                            node_id: Some(node),
                            state: Some("completed".into()),
                            text: Some(text),
                            ..Default::default()
                        },
                        shown,
                    );
                    return Ok(Flow::Ended(0));
                }
                Some(next) if cursor.as_ref() != Some(&next) => cursor = Some(next),
                _ => return Err(ChatError::new(6, "Answer cursor did not advance")),
            }
        }
        Err(ChatError::new(6, "Answer exceeded the bounded page count"))
    }
}

/// Read one message: a trailing backslash continues onto the next line.
/// Returns None at EOF with nothing pending, on `/exit`, or on Ctrl+C.
async fn read_message<R: AsyncBufRead + Unpin, W: Write>(
    input: &mut R,
    out: &mut W,
    prompt: bool,
    interrupts: &mut mpsc::Receiver<()>,
) -> Option<String> {
    let mut message: Vec<String> = Vec::new();
    loop {
        if prompt {
            let _ = write!(out, "{}", if message.is_empty() { "> " } else { "... " });
            let _ = out.flush();
        }
        let mut line = String::new();
        let read = tokio::select! {
            biased;
            _ = interrupts.recv() => return None,
            read = input.read_line(&mut line) => read,
        };
        if !matches!(read, Ok(n) if n > 0) {
            if prompt {
                let _ = writeln!(out);
            }
            break;
        }
        let line = line.trim_end_matches(['\n', '\r']);
        if message.is_empty() && line.trim() == "/exit" {
            return None;
        }
        match line.strip_suffix('\\') {
            Some(rest) => message.push(rest.to_owned()),
            None => {
                message.push(line.to_owned());
                break;
            }
        }
    }
    if message.is_empty() {
        return None;
    }
    Some(message.join("\n"))
}

pub async fn run<R: AsyncBufRead + Unpin, W: Write>(
    options: ChatOptions,
    data: &Path,
    cwd: &Path,
    origin: Option<Origin>,
    mut input: R,
    out: &mut W,
    mut env: ChatEnv,
) -> Result<u8, ChatError> {
    fn path(path: Option<&Path>) -> Result<Option<String>, ChatError> {
        path.map(|p| {
            p.to_str()
                .map(str::to_owned)
                .ok_or_else(|| ChatError::new(2, "Paths must be UTF-8"))
        })
        .transpose()
    }
    let create = ThreadCreateArgs {
        agents: path(options.create.agents.as_deref())?,
        cwd: match path(options.create.cwd.as_deref())? {
            Some(cwd) => Some(cwd),
            None => path(Some(cwd))?,
        },
        connection: options.create.connection.clone(),
        model: options.create.model.clone(),
        effort: options.create.effort.clone(),
        parent_id: options.create.parent_id.clone(),
    };
    Command::ThreadCreate(create.clone())
        .validate()
        .map_err(|e| ChatError::new(2, e.message))?;
    let mut chat = Chat {
        until: options.until,
        timeout: options.timeout_ms.map(Duration::from_millis),
        data: data.into(),
        origin,
        instance: String::new(),
        human: env.stdin_is_tty,
        json: options.json,
        out,
        thread: options.thread_id.clone(),
        run: None,
        turn: None,
        message: None,
    };
    let result: Result<u8, ChatError> = async {
        chat.connect().await?;
        if let Some(id) = chat.thread.clone() {
            chat.emit(
                Event {
                    kind: "thread",
                    state: Some("selected".into()),
                    ..Default::default()
                },
                format!("[thread] {id}"),
            );
        }
        let mut worst = 0u8;
        loop {
            let prompt = env.stdin_is_tty && !options.json;
            let Some(text) =
                read_message(&mut input, &mut *chat.out, prompt, &mut env.interrupts).await
            else {
                return Ok(worst);
            };
            if text.trim().is_empty() {
                continue;
            }
            chat.run = None;
            chat.turn = None;
            match chat.exchange(text, &create, &mut env.interrupts).await? {
                Flow::Exit => return Ok(worst),
                Flow::Ended(code) => worst = worst.max(code),
            }
        }
    }
    .await;
    if let Err(error) = &result {
        chat.emit(
            Event {
                kind: "error",
                summary: Some(error.message.clone()),
                ..Default::default()
            },
            "",
        );
    }
    result
}

fn name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
