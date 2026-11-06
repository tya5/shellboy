//! Interactive line client composed only from existing owner RPCs.
//!
//! It creates or selects a Thread, sends each input through `message.send`,
//! then follows the assigned Turn: it reads the Turn, its shell executions and the
//! saved end message, and between reads long-polls `events.wait` (filtered to its
//! Thread) so it reads again only after the owner says that Thread changed.
//! A hint is never data. The cursor lives in this process only; chat owns no
//! queue, table, worker or state beyond its loop.
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
/// Longest single `events.wait`; a longer deadline simply asks again with the same cursor.
const HINT_POLL_MS: u64 = 30_000;

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
    /// Opaque `events.wait` cursor for `thread`; never parsed. Dropped with this process.
    cursor: Option<String>,
    /// Run whose wait timed out; a prompt-time Ctrl+C stops it if still live.
    timed_out_run: Option<String>,
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
                archived: Default::default(),
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
        // Take the cursor before sending so a change between send and the first
        // read is reported by the first wait instead of being missed.
        self.baseline().await?;
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

    /// Current cursor for the shown Thread (`events.wait` without `after` returns at once).
    async fn baseline(&mut self) -> Result<(), ChatError> {
        let result: EventsWaitResult = self
            .call(Command::EventsWait(EventsWaitArgs {
                after: None,
                thread_ids: self.thread.clone().into_iter().collect(),
                timeout_ms: 0,
                include_text: false,
            }))
            .await?;
        self.cursor = Some(result.cursor);
        Ok(())
    }

    /// Wait for a hint about the shown Thread and return the cursor to continue with.
    /// Every outcome (`changed`, `timeout`, `resync_required`) means "read again": a
    /// timeout is the safety net that bounds the damage of a producer that sent no hint.
    /// `more` is drained at once; each call advances the cursor.
    async fn hint(&self, mut after: String, timeout_ms: u64) -> Result<String, ChatError> {
        let mut timeout_ms = timeout_ms;
        loop {
            let result: EventsWaitResult = self
                .call(Command::EventsWait(EventsWaitArgs {
                    after: Some(after),
                    thread_ids: self.thread.clone().into_iter().collect(),
                    timeout_ms,
                    include_text: false,
                }))
                .await?;
            after = result.cursor;
            if !result.more {
                return Ok(after);
            }
            timeout_ms = 0;
        }
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
        let mut turn = turn.to_owned();
        let mut interrupts_open = true;
        loop {
            // Run-scoped waits follow the Run's current Turn, which can change.
            let run_facts: Option<RunDetails> = if self.until == ResultMode::TurnEnd {
                None
            } else {
                let run = self
                    .run
                    .clone()
                    .ok_or_else(|| ChatError::new(6, "no Run"))?;
                Some(
                    self.call(Command::RunGet(RunTarget { run_id: run }))
                        .await?,
                )
            };
            if let Some(current) = run_facts.as_ref().and_then(|r| r.turn_id.clone()) {
                turn = current;
            }
            let facts: TurnDetails = self
                .call(Command::TurnGet(TurnTarget {
                    turn_id: turn.clone(),
                }))
                .await?;
            self.turn = Some(turn.clone());
            self.tool_progress(&turn, &mut tools).await?;
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
                return Ok(self.recovery_wait(&facts.recovery_hint.clone().unwrap_or_default()));
            }
            if self.until == ResultMode::TurnEnd {
                match facts.state {
                    TurnState::Completed => {
                        return self.completed(facts.end_message_node_id).await;
                    }
                    TurnState::Stopped | TurnState::Failed => {
                        return Ok(self.abnormal(&name(&facts.state)));
                    }
                    _ => {}
                }
            } else if let Some(run_facts) = run_facts {
                match run_facts.state {
                    RunState::Completed if self.until == ResultMode::RunEnd => {
                        return self.completed(run_facts.end_message_node_id).await;
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
                        self.timed_out_run = self.run.clone();
                        return Ok(Flow::Ended(5));
                    }
                    (left.as_millis().clamp(1, HINT_POLL_MS as u128)) as u64
                }
                None => HINT_POLL_MS,
            };
            let after = self
                .cursor
                .clone()
                .ok_or_else(|| ChatError::new(6, "no events cursor"))?;
            let wait = self.hint(after, remaining);
            tokio::select! {
                biased;
                signal = interrupts.recv(), if interrupts_open => {
                    if signal.is_none() {
                        interrupts_open = false;
                        continue;
                    }
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
                result = wait => {
                    // Every outcome is an observation; the next pass reads saved facts.
                    self.cursor = Some(result?);
                }
            }
        }
    }

    /// True when a still-live Run was asked to stop.
    async fn stop_timed_out_run(&mut self) -> Result<bool, ChatError> {
        let Some(run) = self.timed_out_run.take() else {
            return Ok(false);
        };
        let facts: RunDetails = self
            .call(Command::RunGet(RunTarget {
                run_id: run.clone(),
            }))
            .await?;
        if facts.state != RunState::Live {
            return Ok(false);
        }
        let _: Value = self
            .call(Command::RunStop(RunTarget {
                run_id: run.clone(),
            }))
            .await?;
        self.run = Some(run);
        self.emit(
            Event {
                kind: "run_stopping",
                state: Some("stopping".into()),
                ..Default::default()
            },
            "[stop] stopping the Run that timed out; press Ctrl+C again to leave chat",
        );
        Ok(true)
    }

    fn recovery_wait(&mut self, hint: &str) -> Flow {
        let hint = short(hint);
        self.emit(
            Event {
                kind: "notice",
                state: Some("recovery_wait".into()),
                summary: Some(hint.clone()),
                ..Default::default()
            },
            format!(
                "[notice] the Turn needs explicit recovery (shellboy run retry --run RUN_ID); not retried automatically. {hint}"
            ),
        );
        Flow::Ended(4)
    }

    /// A completed Turn or Run proves the answer only if this message was applied.
    async fn completed(&mut self, node: Option<String>) -> Result<Flow, ChatError> {
        let details: MessageDetails = self
            .call(Command::MessageGet(MessageTarget {
                message_id: self.message.clone().unwrap_or_default(),
            }))
            .await?;
        if details.delivery_status != DeliveryStatus::Applied {
            return Ok(self.held(&details));
        }
        self.end_message(node).await
    }

    fn held(&mut self, details: &MessageDetails) -> Flow {
        let state = name(&details.delivery_status);
        self.emit(
            Event {
                kind: "notice",
                state: Some(state.clone()),
                summary: Some("message was not applied by the finished Turn".into()),
                ..Default::default()
            },
            format!(
                "[notice] message {} is {state}, not applied; the finished Turn is not its answer. Inspect it with `shellboy message get --message {}`",
                details.message_id, details.message_id
            ),
        );
        Flow::Ended(4)
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
        if details.delivery_status != DeliveryStatus::Applied {
            return Ok(self.held(&details));
        }
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
            let known = item["started_at_known"].as_bool().unwrap_or(false);
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
                        at: known.then(|| text_of(&item["started_at"])).flatten(),
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
                        at: known.then(|| text_of(&item["ended_at"])).flatten(),
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
        let Some(node) = node else {
            return Ok(self.abnormal("no_end_message"));
        };
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
                return Ok(self.abnormal("end_message_unavailable"));
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

enum Read {
    Message(String),
    /// EOF with nothing pending, or `/exit`.
    Done,
    Interrupt,
}

/// Read one message: a trailing backslash continues onto the next line.
/// A Ctrl+C is reported to the caller, which decides whether it stops a Run or exits.
async fn read_message<R: AsyncBufRead + Unpin, W: Write>(
    input: &mut R,
    out: &mut W,
    prompt: bool,
    interrupts: &mut mpsc::Receiver<()>,
) -> Read {
    let mut message: Vec<String> = Vec::new();
    loop {
        if prompt {
            let _ = write!(out, "{}", if message.is_empty() { "> " } else { "... " });
            let _ = out.flush();
        }
        let mut line = String::new();
        let read = tokio::select! {
            biased;
            _ = interrupts.recv() => return Read::Interrupt,
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
            return Read::Done;
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
        return Read::Done;
    }
    Read::Message(message.join("\n"))
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
        cursor: None,
        timed_out_run: None,
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
            let text =
                match read_message(&mut input, &mut *chat.out, prompt, &mut env.interrupts).await {
                    Read::Message(text) => text,
                    Read::Done => return Ok(worst),
                    Read::Interrupt => {
                        // After a timeout the Run may still be live: the first Ctrl+C
                        // stops it (TUI parity), a second one leaves.
                        if chat.stop_timed_out_run().await? {
                            continue;
                        }
                        return Ok(worst);
                    }
                };
            if text.trim().is_empty() {
                continue;
            }
            chat.run = None;
            chat.turn = None;
            chat.timed_out_run = None;
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
