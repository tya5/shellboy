//! Human/model CLI syntax. Conversion produces the same typed operations as TUI.
use crate::protocol::{self as wire, ApiError, ErrorCode};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::{Component, Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const MAX_STDIN_TEXT_BYTES: usize = wire::MAX_FRAME_BYTES;

#[derive(Debug, Clone, Parser)]
#[command(
    name = "shellboy",
    version,
    about = "Native conversational agent",
    disable_help_subcommand = false,
    after_help = "Start here:\n  shellboy docs list\n  shellboy docs search --query background\n  shellboy docs get --id shellboy/agent-guide\n\nUse docs for operation guides, history for conversation evidence, and rag for explicit document/history search. Owner commands require the matching --data-dir; docs and rag do not start an owner."
)]
pub struct Cli {
    /// Data directory for the owner and RAG defaults; independent from shell/thread cwd.
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    /// Configuration file for the TUI owner or standalone RAG embeddings.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    /// Request ID. Mutations replay saved outcomes; observations read current state without a receipt.
    #[arg(long, global = true)]
    pub operation_id: Option<String>,
    #[command(subcommand)]
    pub command: Option<CliCommand>,
}
#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    /// Inspect recorded work; no model call or persistent job engine.
    Status(StatusOptions),
    /// Read and search the guides bundled with this executable; no owner or API needed.
    Docs {
        #[command(subcommand)]
        command: crate::docs::DocsCommand,
    },
    /// Index documents and search explicit sources without starting an agent.
    Rag(crate::rag::RagOptions),
    /// List models advertised by an owner-configured connection.
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// List, inspect, create, fork, delete, or configure conversations.
    Thread {
        #[command(subcommand)]
        command: ThreadCommand,
    },
    /// Submit text or inspect its immutable assignment.
    Message {
        #[command(subcommand)]
        command: MessageCommand,
    },
    /// Inspect a previously accepted operation receipt.
    Operation {
        #[command(subcommand)]
        command: OperationCommand,
    },
    /// Inspect execution lifetime, explicitly recover a waiting Turn, or request stop.
    Run {
        #[command(subcommand)]
        command: RunCommand,
    },
    /// Inspect one answer interval and its saved answer node.
    Turn {
        #[command(subcommand)]
        command: TurnCommand,
    },
    /// Wait for any condition; timeout does not stop the target.
    Wait(WaitOptions),
    /// Inspect original history and search the active embedding space.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Inspect and control existing shell executions; shell start is a model tool.
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },
    /// Add, list, or remove path references for a conversation.
    Attachment {
        #[command(subcommand)]
        command: AttachmentCommand,
    },
    /// Attach a path; shorthand for attachment add --path PATH.
    Attach(AttachOptions),
    /// Request compaction at the next safe boundary.
    Compact(CompactOptions),
    /// Inspect and manage the owner conversation embedding index.
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    /// Inspect retention and explicitly collect eligible stored data.
    Storage {
        #[command(subcommand)]
        command: StorageCommand,
    },
}
#[derive(Debug, Clone, Subcommand)]
pub enum ModelCommand {
    List(ModelListOptions),
}
#[derive(Debug, Clone, Args)]
pub struct ModelListOptions {
    /// Connection name; otherwise use the target thread or owner default.
    #[arg(long)]
    pub connection: Option<String>,
    /// Thread whose connection to use; defaults to the authenticated caller.
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
}
#[derive(Debug, Clone, Subcommand)]
pub enum ThreadCommand {
    List(ListOptions),
    Get(ThreadOptions),
    /// Create a saved Thread; send a message separately to start work.
    #[command(
        after_help = "Delegate from a parent model shell:\n  shellboy thread create --parent THREAD_ID\n  shellboy message send --thread CHILD_THREAD_ID --text 'Read-only audit; return findings and evidence'\n\nRead result.thread_id from create, then track the separate message_id/run_id/turn_id from send. Guide: shellboy docs get --id shellboy/delegation"
    )]
    Create(ThreadCreateOptions),
    Fork(ThreadForkOptions),
    Delete(ThreadDeleteOptions),
    /// Inspect deletion blockers and direct references without deleting anything.
    DeletePreview(ThreadDeleteOptions),
    Set(ThreadSetOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum MessageCommand {
    /// Save an instruction; acceptance is separate from execution and answer completion.
    #[command(
        after_help = "Submit and follow the returned assignment:\n  shellboy message send --thread THREAD_ID --text 'Summarize the findings'\n  shellboy message get --message MESSAGE_ID\n  shellboy turn get --turn TURN_ID\n  shellboy history get --node ANSWER_NODE_ID\n\nUse only IDs returned for this request. pending has no assigned Turn yet; inspect the same message instead of sending it again. Guide: shellboy docs get --id shellboy/delegation"
    )]
    Send(MessageSendOptions),
    Get(MessageOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum OperationCommand {
    /// Inspect a receipt and downstream targets before repeating an uncertain mutation.
    #[command(
        after_help = "  shellboy operation get --operation OPERATION_ID\n\nUse error.details.operation_id if an automatically assigned ID was returned after a connection failure. An unavailable receipt does not prove the side effect never happened. This lookup allocates no receipt. --operation selects the saved receipt; global --operation-id only identifies this lookup request. Guide: shellboy docs get --id shellboy/recovery-and-storage"
    )]
    Get(OperationOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum RunCommand {
    /// Inspect native state, current Turn, ownership, and saved recovery reason.
    Get(RunOptions),
    /// Explicitly retry a recovery-wait Turn after inspecting and fixing its cause.
    #[command(
        after_help = "Inspect first: shellboy run get --run RUN_ID\nGuide: shellboy docs get --id shellboy/recovery-and-storage\nOnly an authorized direct owner or human chooses retry. Terminal Runs are not revived; do not repeat an unconfirmed side effect under a new ID."
    )]
    Retry(RunOptions),
    /// Request stopping this Run and its owned work; inspect state to confirm termination.
    Stop(RunOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum TurnCommand {
    /// Inspect state and answer_node_id; fetch the answer with history get.
    #[command(
        after_help = "  shellboy turn get --turn TURN_ID\n  shellboy history get --node ANSWER_NODE_ID\n\nFetch only a returned answer_node_id; stopped/failed terminal states need not have an answer. Guide: shellboy docs get --id shellboy/delegation"
    )]
    Get(TurnOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum HistoryCommand {
    List(HistoryListOptions),
    Get(HistoryGetOptions),
    Search(SearchOptions),
    Relations(HistoryRelationsOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum ShellCommand {
    /// Inspect the original command, recorded context, native state, and output completion.
    #[command(
        after_help = "  shellboy shell get --execution WORK_EXECUTION_ID\n  shellboy shell read --execution WORK_EXECUTION_ID --limit-bytes 65536\n\nWORK_EXECUTION_ID comes from the shell tool that started the work. The shell running this observation has a different ID. Guide: shellboy docs get --id shellboy/shell"
    )]
    Get(ExecutionOptions),
    /// Read saved raw output for this execution; this does not start or retry work.
    #[command(
        after_help = "  shellboy shell read --execution WORK_EXECUTION_ID --limit-bytes 65536\n  shellboy shell read --execution WORK_EXECUTION_ID --cursor CURSOR --limit-bytes 65536\n\nUse result.cursor from the preceding read. Keep the same execution and encoding. Check result.text/raw_bytes/output_complete and shell get state. A send CLI process can legitimately output message acceptance JSON; its execution ID is separate from the requested work. Guide: shellboy docs get --id shellboy/shell"
    )]
    Read(ShellReadOptions),
    Write(ShellWriteOptions),
    Eof(ExecutionOptions),
    Stop(ExecutionOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum AttachmentCommand {
    Add(AttachmentAddOptions),
    List(ThreadOptions),
    Remove(AttachmentRemoveOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum IndexCommand {
    Status(IndexOptions),
    /// Inspect candidate counts without embedding requests or starting jobs.
    Preview(IndexOptions),
    /// Start paid indexing of explicitly selected history. Check preview first.
    Backfill(IndexOptions),
    Retry(IndexOptions),
    /// Build a candidate index; embedding API costs may apply. Check preview first.
    Rebuild(IndexOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum StorageCommand {
    Status(StorageOptions),
    Collect(StorageOptions),
}

#[derive(Debug, Clone, Args)]
pub struct ListOptions {
    #[arg(long,default_value_t=wire::DEFAULT_LIST_LIMIT)]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub query: Option<String>,
}
#[derive(Debug, Clone, Args)]
#[command(
    after_help = "Examples:\n  shellboy status\n  shellboy operation get --operation OPERATION_ID\n  shellboy status --thread THREAD\n  shellboy status --run RUN --all\n  shellboy status --state failed --since 2026-10-25T00:00:00Z\n\nSaved database facts only; OS processes and independent RAG searches are not observed.\nThe deprecated status --operation alias uses operation get and will be removed after v0.1.2.\nGuide: shellboy docs get --id shellboy/agent-guide"
)]
pub struct StatusOptions {
    /// Deprecated until the next release: use operation get --operation ID.
    #[arg(
        long = "operation",
        id = "status_lookup_operation_id",
        conflicts_with_all = ["thread_id", "run_id", "since", "state", "all", "limit", "cursor"]
    )]
    pub operation_id: Option<String>,
    /// Exact Thread; model callers default to their own Thread.
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    /// Exact owning Run; does not implicitly include owned child Runs.
    #[arg(long = "run")]
    pub run_id: Option<String>,
    /// Recorded activity at or after RFC3339 time; not all state transitions are timestamped.
    #[arg(long)]
    pub since: Option<String>,
    /// Exact native state, repeatable; includes matching terminal states.
    #[arg(long = "state")]
    pub state: Vec<String>,
    /// Include terminal work as well as pending work.
    #[arg(long)]
    pub all: bool,
    #[arg(long, default_value_t = wire::DEFAULT_LIST_LIMIT)]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, Args)]
pub struct ThreadOptions {
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
}
#[derive(Debug, Clone, Args)]
#[command(group(clap::ArgGroup::new("edit_action").args(["spill", "summary", "trim", "omit", "restore"]).multiple(false)))]
pub struct CompactOptions {
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    /// Spill completed tool groups in the exact inclusive node range; no model request.
    #[arg(long)]
    pub spill: bool,
    /// Summarize the exact inclusive node range using the configured model.
    #[arg(long)]
    pub summary: bool,
    /// Remove the selected prefix through --to, retaining provenance.
    #[arg(long)]
    pub trim: bool,
    /// Omit the exact inclusive node range, keeping the earlier and later path.
    #[arg(long)]
    pub omit: bool,
    /// Restore a trim marker (latest by default); retain all later nodes.
    #[arg(long)]
    pub restore: bool,
    /// Inclusive first source node; --restore also accepts a selected marker or its first omitted source.
    #[arg(long = "from", requires = "edit_action")]
    pub from_node_id: Option<String>,
    /// Inclusive last source node, or removed prefix end with --trim.
    #[arg(long = "to", requires = "edit_action")]
    pub to_node_id: Option<String>,
}
#[derive(Debug, Clone, Args)]
pub struct ThreadCreateOptions {
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    #[arg(long)]
    pub connection: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Empty string explicitly clears effort; omission inherits the default.
    #[arg(long)]
    pub effort: Option<String>,
    #[arg(long = "parent")]
    pub parent_id: Option<String>,
}
#[derive(Debug, Clone, Args)]
pub struct ThreadForkOptions {
    #[arg(long = "thread")]
    pub thread_id: String,
    #[arg(long = "node")]
    pub node_id: String,
    #[arg(long)]
    pub cwd: Option<PathBuf>,
}
#[derive(Debug, Clone, Args)]
pub struct ThreadDeleteOptions {
    #[arg(long = "thread")]
    pub thread_id: String,
    #[arg(long)]
    pub descendants: bool,
}
#[derive(Debug, Clone, Args)]
pub struct ThreadSetOptions {
    /// Display title; an empty string restores the automatic title.
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    #[arg(long)]
    pub connection: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Empty string explicitly clears effort.
    #[arg(long)]
    pub effort: Option<String>,
}
#[derive(Debug, Clone, Args)]
#[group(required = true, multiple = false)]
pub struct TextOptions {
    #[arg(long, conflicts_with = "text_stdin")]
    pub text: Option<String>,
    /// Read UTF-8 text from stdin, with a hard byte bound and no truncation.
    #[arg(long, conflicts_with = "text")]
    pub text_stdin: bool,
}
#[derive(Debug, Clone, Args)]
pub struct MessageSendOptions {
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    #[command(flatten)]
    pub text: TextOptions,
}
#[derive(Debug, Clone, Args)]
pub struct MessageOptions {
    #[arg(long = "message")]
    pub message_id: String,
}
#[derive(Debug, Clone, Args)]
pub struct OperationOptions {
    // A global argument with the same clap ID would propagate this lookup target
    // into Cli.operation_id and conflict with the operation being inspected.
    #[arg(long = "operation", id = "lookup_operation_id")]
    pub operation_id: String,
}
#[derive(Debug, Clone, Args)]
pub struct RunOptions {
    #[arg(long = "run")]
    pub run_id: String,
}
#[derive(Debug, Clone, Args)]
pub struct TurnOptions {
    #[arg(long = "turn")]
    pub turn_id: String,
}
#[derive(Debug, Clone, Args)]
pub struct ExecutionOptions {
    /// ID returned by the shell tool that started the target command.
    #[arg(long = "execution")]
    pub execution_id: String,
}
#[derive(Debug, Clone, Args)]
#[command(
    after_help = "Wait for a saved target:\n  shellboy wait --conditions-json '[{\"type\":\"shell_ended\",\"execution_id\":\"WORK_EXECUTION_ID\"}]' --timeout-ms 30000\n  shellboy wait --conditions-json '[{\"type\":\"turn_completed\",\"turn_id\":\"CHILD_TURN_ID\"}]' --timeout-ms 30000\n\nConditions use any semantics. timeout leaves the target running; satisfied means inspect target state, not guaranteed success. recovery_wait means inspect Run/Turn and choose recovery explicitly. Guide: shellboy docs get --id shellboy/delegation"
)]
pub struct WaitOptions {
    /// JSON array of typed turn_completed/run_ended/shell_ended/shell_output conditions.
    #[arg(long)]
    pub conditions_json: String,
    #[arg(long, default_value_t = wire::DEFAULT_WAIT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}
#[derive(Debug, Clone, Args)]
pub struct HistoryListOptions {
    #[arg(long = "thread")]
    pub thread_id: String,
    #[arg(long,default_value_t=wire::DEFAULT_LIST_LIMIT)]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
    /// Originating Run ID, including shared nodes selected by a fork.
    #[arg(long = "run")]
    pub run_id: Option<String>,
    /// Inclusive node creation time, RFC3339 with timezone.
    #[arg(long)]
    pub since: Option<String>,
    /// Related shell's saved native state (starting/running/stopping/exited/failed/stopped).
    #[arg(long = "status")]
    pub state: Option<String>,
}
#[derive(Debug, Clone, Args)]
pub struct HistoryGetOptions {
    #[arg(long = "node")]
    pub node_id: String,
    #[arg(long,default_value_t=wire::MAX_BODY_BYTES)]
    pub limit_bytes: u32,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long, default_value = wire::DEFAULT_ENCODING)]
    pub encoding: String,
}
impl ValueEnum for wire::HistoryRelation {
    fn value_variants<'a>() -> &'a [Self] {
        Self::VALUES
    }
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.as_str()))
    }
}
#[derive(Debug, Clone, Args)]
pub struct HistoryRelationsOptions {
    #[arg(long = "node")]
    pub node_id: String,
    #[arg(long, value_enum)]
    pub relation: wire::HistoryRelation,
    #[arg(long, default_value_t = wire::DEFAULT_LIST_LIMIT)]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, Args)]
pub struct SearchOptions {
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
    #[arg(long = "exclude-kind")]
    pub exclude_kinds: Vec<String>,
    #[arg(long)]
    pub from: Option<String>,
    #[arg(long)]
    pub to: Option<String>,
    #[arg(long)]
    pub query: String,
    #[arg(long, default_value_t = wire::DEFAULT_SEARCH_LIMIT)]
    pub limit: u32,
}
#[derive(Debug, Clone, Args)]
pub struct ShellReadOptions {
    /// Target work ID from shell start, separate from this observation shell ID.
    #[arg(long = "execution")]
    pub execution_id: String,
    /// result.cursor from the previous read of the same execution and encoding.
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub encoding: Option<String>,
    #[arg(long,default_value_t=wire::MAX_BODY_BYTES)]
    pub limit_bytes: u32,
}
#[derive(Debug, Clone, Args)]
pub struct ShellWriteOptions {
    #[arg(long = "execution")]
    pub execution_id: String,
    #[command(flatten)]
    pub text: TextOptions,
    #[arg(long, default_value = wire::DEFAULT_ENCODING)]
    pub encoding: String,
}
// CLI spelling belongs here; protocol owns the vocabulary and wire names.
pub type AttachmentType = wire::AttachmentKind;
impl ValueEnum for wire::AttachmentKind {
    fn value_variants<'a>() -> &'a [Self] {
        Self::VALUES
    }
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.as_str()))
    }
}
#[derive(Debug, Clone, Args)]
pub struct AttachmentAddOptions {
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    #[arg(long)]
    pub path: PathBuf,
    #[arg(long, value_enum)]
    pub kind: AttachmentType,
}
#[derive(Debug, Clone, Args)]
pub struct AttachOptions {
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    pub path: PathBuf,
    #[arg(long, value_enum)]
    pub kind: AttachmentType,
}
#[derive(Debug, Clone, Args)]
pub struct AttachmentRemoveOptions {
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    #[arg(long = "attachment")]
    pub attachment_id: String,
}
// CLI spelling belongs here; protocol owns the vocabulary and wire names.
pub type Scope = wire::IndexScope;
impl ValueEnum for wire::IndexScope {
    fn value_variants<'a>() -> &'a [Self] {
        Self::VALUES
    }
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.as_str()))
    }
}
#[derive(Debug, Clone, Args)]
pub struct IndexOptions {
    #[arg(long, value_enum)]
    pub scope: Option<Scope>,
    #[arg(long = "space")]
    pub space_id: Option<String>,
    /// Repeat to explicitly select historical conversations for indexing.
    #[arg(long = "thread")]
    pub thread_ids: Vec<String>,
    #[arg(long = "source")]
    pub source_ids: Vec<String>,
}
impl From<IndexOptions> for wire::IndexArgs {
    fn from(value: IndexOptions) -> Self {
        Self {
            scope: value.scope,
            space_id: value.space_id,
            thread_ids: value.thread_ids,
            source_ids: value.source_ids,
        }
    }
}
// CLI spelling belongs here; protocol owns the vocabulary and wire names.
pub type Category = wire::StorageCategory;
impl ValueEnum for wire::StorageCategory {
    fn value_variants<'a>() -> &'a [Self] {
        Self::VALUES
    }
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(match self {
            Self::ManagedImages => "managed-images",
            _ => self.as_str(),
        }))
    }
}
#[derive(Debug, Clone, Args)]
pub struct StorageOptions {
    #[arg(long, value_enum)]
    pub category: Option<Category>,
}
impl From<StorageOptions> for wire::StorageArgs {
    fn from(value: StorageOptions) -> Self {
        Self {
            category: value.category,
        }
    }
}

impl Cli {
    pub async fn into_command(self, startup_cwd: &Path) -> Result<Option<wire::Command>, ApiError> {
        self.into_command_with_stdin(startup_cwd, &mut tokio::io::stdin())
            .await
    }
    /// Injectable reader avoids process-global stdin manipulation in tests and
    /// permits callers to keep input handling outside the UI OS thread.
    pub async fn into_command_with_stdin<R: AsyncRead + Unpin>(
        self,
        startup_cwd: &Path,
        stdin: &mut R,
    ) -> Result<Option<wire::Command>, ApiError> {
        use wire::Command as C;
        let Some(command) = self.command else {
            return Ok(None);
        };
        let command = match command {
            CliCommand::Status(a) => {
                if let Some(operation_id) = a.operation_id {
                    if a.thread_id.is_some()
                        || a.run_id.is_some()
                        || a.since.is_some()
                        || !a.state.is_empty()
                        || a.all
                        || a.limit != wire::DEFAULT_LIST_LIMIT
                        || a.cursor.is_some()
                    {
                        return Err(invalid(
                            "status --operation cannot be combined with work filters, a limit, or a cursor; use operation get --operation ID",
                        ));
                    }
                    C::OperationGet(wire::OperationTarget { operation_id })
                } else {
                    C::Status(wire::StatusArgs {
                        operation_id: None,
                        thread_id: a.thread_id,
                        run_id: a.run_id,
                        since: a.since,
                        state: a.state,
                        all: a.all,
                        limit: a.limit,
                        cursor: a.cursor,
                    })
                }
            }
            CliCommand::Docs { .. } | CliCommand::Rag(_) => return Ok(None),
            CliCommand::Model {
                command: ModelCommand::List(a),
            } => C::ModelList(wire::ModelListArgs {
                connection: a.connection,
                thread_id: a.thread_id,
            }),
            CliCommand::Thread { command } => match command {
                ThreadCommand::List(a) => C::ThreadList(wire::ThreadListArgs {
                    limit: a.limit,
                    cursor: a.cursor,
                    query: a.query,
                }),
                ThreadCommand::Get(a) => C::ThreadGet(wire::ThreadTarget {
                    thread_id: a.thread_id,
                }),
                ThreadCommand::Create(a) => C::ThreadCreate(wire::ThreadCreateArgs {
                    connection: a.connection,
                    cwd: path_option(a.cwd)?,
                    model: a.model,
                    effort: a.effort,
                    parent_id: a.parent_id,
                }),
                ThreadCommand::Fork(a) => C::ThreadFork(wire::ThreadForkArgs {
                    thread_id: a.thread_id,
                    node_id: a.node_id,
                    cwd: path_option(a.cwd)?,
                }),
                ThreadCommand::Delete(a) => C::ThreadDelete(wire::ThreadDeleteArgs {
                    thread_id: a.thread_id,
                    descendants: a.descendants,
                }),
                ThreadCommand::DeletePreview(a) => C::ThreadDeletePreview(wire::ThreadDeleteArgs {
                    thread_id: a.thread_id,
                    descendants: a.descendants,
                }),
                ThreadCommand::Set(a) => C::ThreadSet(wire::ThreadSetArgs {
                    title: a.title,
                    connection: a.connection,
                    thread_id: a.thread_id,
                    cwd: path_option(a.cwd)?,
                    model: a.model,
                    effort: a.effort,
                }),
            },
            CliCommand::Message { command } => match command {
                MessageCommand::Send(a) => C::MessageSend(wire::MessageSendArgs {
                    thread_id: a.thread_id,
                    text: read_text(a.text, stdin).await?,
                }),
                MessageCommand::Get(a) => C::MessageGet(wire::MessageTarget {
                    message_id: a.message_id,
                }),
            },
            CliCommand::Operation {
                command: OperationCommand::Get(a),
            } => C::OperationGet(wire::OperationTarget {
                operation_id: a.operation_id,
            }),
            CliCommand::Run { command } => match command {
                RunCommand::Get(a) => C::RunGet(wire::RunTarget { run_id: a.run_id }),
                RunCommand::Retry(a) => C::RunRetry(wire::RunTarget { run_id: a.run_id }),
                RunCommand::Stop(a) => C::RunStop(wire::RunTarget { run_id: a.run_id }),
            },
            CliCommand::Turn {
                command: TurnCommand::Get(a),
            } => C::TurnGet(wire::TurnTarget { turn_id: a.turn_id }),
            CliCommand::Wait(a) => C::Wait(wire::WaitArgs {
                conditions: serde_json::from_str(&a.conditions_json).map_err(|_| {
                    invalid("conditions-json must be an array of typed wait conditions")
                })?,
                timeout_ms: a.timeout_ms,
            }),
            CliCommand::History { command } => match command {
                HistoryCommand::List(a) => C::HistoryList(wire::HistoryListArgs {
                    thread_id: a.thread_id,
                    limit: a.limit,
                    cursor: a.cursor,
                    run_id: a.run_id,
                    since: a.since,
                    state: a.state,
                }),
                HistoryCommand::Get(a) => C::HistoryGet(wire::HistoryGetArgs {
                    node_id: a.node_id,
                    limit_bytes: a.limit_bytes,
                    cursor: a.cursor,
                    encoding: a.encoding,
                }),
                HistoryCommand::Search(a) => C::HistorySearch(wire::HistorySearchArgs {
                    query: a.query,
                    limit: a.limit,
                    thread_id: a.thread_id,
                    kinds: a.kinds,
                    exclude_kinds: a.exclude_kinds,
                    from: a.from,
                    to: a.to,
                }),
                HistoryCommand::Relations(a) => C::HistoryRelations(wire::HistoryRelationsArgs {
                    node_id: a.node_id,
                    relation: a.relation,
                    limit: a.limit,
                    cursor: a.cursor,
                }),
            },
            CliCommand::Shell { command } => match command {
                ShellCommand::Get(a) => C::ShellGet(wire::ExecutionTarget {
                    execution_id: a.execution_id,
                }),
                ShellCommand::Read(a) => C::ShellRead(wire::ShellReadArgs {
                    execution_id: a.execution_id,
                    cursor: a.cursor,
                    encoding: a.encoding,
                    limit_bytes: a.limit_bytes,
                }),
                ShellCommand::Write(a) => C::ShellWrite(wire::ShellWriteArgs {
                    execution_id: a.execution_id,
                    text: read_text(a.text, stdin).await?,
                    encoding: a.encoding,
                }),
                ShellCommand::Eof(a) => C::ShellEof(wire::ExecutionTarget {
                    execution_id: a.execution_id,
                }),
                ShellCommand::Stop(a) => C::ShellStop(wire::ExecutionTarget {
                    execution_id: a.execution_id,
                }),
            },
            CliCommand::Attachment { command } => match command {
                AttachmentCommand::Add(a) => C::AttachmentAdd(wire::AttachmentAddArgs {
                    thread_id: a.thread_id,
                    path: absolute_attachment(a.path, startup_cwd)?,
                    kind: a.kind,
                }),
                AttachmentCommand::List(a) => C::AttachmentList(wire::ThreadTarget {
                    thread_id: a.thread_id,
                }),
                AttachmentCommand::Remove(a) => C::AttachmentRemove(wire::AttachmentRemoveArgs {
                    thread_id: a.thread_id,
                    attachment_id: a.attachment_id,
                }),
            },
            CliCommand::Attach(a) => C::AttachmentAdd(wire::AttachmentAddArgs {
                thread_id: a.thread_id,
                path: absolute_attachment(a.path, startup_cwd)?,
                kind: a.kind,
            }),
            CliCommand::Compact(a) => {
                let action = if a.spill {
                    Some(wire::ContextEditAction::Spill)
                } else if a.summary {
                    Some(wire::ContextEditAction::Summary)
                } else if a.trim {
                    Some(wire::ContextEditAction::Trim)
                } else if a.omit {
                    Some(wire::ContextEditAction::Omit)
                } else if a.restore {
                    Some(wire::ContextEditAction::Restore)
                } else {
                    None
                };
                match action {
                    Some(action) => C::ContextEdit(wire::ContextEditArgs {
                        thread_id: a.thread_id,
                        action,
                        from_node_id: a.from_node_id,
                        to_node_id: a.to_node_id,
                    }),
                    None => C::Compact(wire::ThreadTarget {
                        thread_id: a.thread_id,
                    }),
                }
            }
            CliCommand::Index { command } => match command {
                IndexCommand::Status(a) => C::IndexStatus(a.into()),
                IndexCommand::Preview(a) => C::IndexPreview(a.into()),
                IndexCommand::Backfill(a) => C::IndexBackfill(a.into()),
                IndexCommand::Retry(a) => C::IndexRetry(a.into()),
                IndexCommand::Rebuild(a) => C::IndexRebuild(a.into()),
            },
            CliCommand::Storage { command } => match command {
                StorageCommand::Status(a) => C::StorageStatus(a.into()),
                StorageCommand::Collect(a) => C::StorageCollect(a.into()),
            },
        };
        command.validate()?;
        Ok(Some(command))
    }
}
fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}
async fn read_text<R: AsyncRead + Unpin>(
    options: TextOptions,
    reader: &mut R,
) -> Result<String, ApiError> {
    match (options.text, options.text_stdin) {
        (Some(text), false) => {
            if text.len() > MAX_STDIN_TEXT_BYTES {
                Err(ApiError::new(
                    ErrorCode::InputLimit,
                    "text exceeds the CLI byte limit",
                ))
            } else {
                Ok(text)
            }
        }
        (None, true) => {
            let mut bytes = Vec::new();
            reader
                .take((MAX_STDIN_TEXT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| ApiError::new(ErrorCode::IoFailed, "cannot read text from stdin"))?;
            if bytes.len() > MAX_STDIN_TEXT_BYTES {
                return Err(ApiError::new(
                    ErrorCode::InputLimit,
                    "stdin text exceeds the CLI byte limit",
                ));
            }
            String::from_utf8(bytes).map_err(|_| invalid("stdin text must be valid UTF-8"))
        }
        _ => Err(invalid("specify exactly one of --text and --text-stdin")),
    }
}
fn path_string(path: PathBuf) -> Result<String, ApiError> {
    crate::input_path::utf8(&path)
        .map(str::to_owned)
        .map_err(invalid)
}
fn path_option(path: Option<PathBuf>) -> Result<Option<String>, ApiError> {
    path.map(path_string).transpose()
}
fn absolute_attachment(path: PathBuf, cwd: &Path) -> Result<String, ApiError> {
    if path.as_os_str().is_empty() {
        return Err(invalid("attachment path must not be empty"));
    }
    if !cwd.is_absolute() {
        return Err(invalid("CLI startup cwd must be absolute"));
    }
    let absolute = crate::input_path::relative_to(&path, cwd);
    // Lexical cleanup only: no metadata, symlink resolution, or existence check.
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    if !normalized.is_absolute() {
        return Err(invalid(
            "attachment path must resolve to an absolute path; drive-relative paths require an absolute path",
        ));
    }
    path_string(normalized)
}
