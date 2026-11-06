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
    after_help = "Start here:\n  shellboy docs list\n  shellboy docs search --query background\n  shellboy docs get --id shellboy/agent-guide\n\nUse docs for operation guides, history for conversation evidence, and rag for explicit document/history search. Owner commands require the matching --data-dir; docs, startup and rag do not start an owner."
)]
pub struct Cli {
    /// Data directory for the owner and RAG defaults; independent from shell/thread cwd.
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    /// Configuration file for the owner or standalone RAG embeddings.
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
    /// Start an owner without a terminal; requires an explicit --data-dir.
    Serve,
    /// Ask a terminal-free owner to shut down; requires an explicit --data-dir.
    Shutdown,
    /// Run saved work noninteractively; validate only its final JSON answer locally.
    Exec(crate::exec_client::ExecOptions),
    /// Inspect recorded work; no model call or persistent job engine.
    Status(StatusOptions),
    /// Create a configuration template or check it without an owner.
    Config {
        #[command(subcommand)]
        command: crate::config_setup::ConfigCommand,
    },
    /// Read saved model usage and separate timing totals; no model call.
    Usage {
        #[command(subcommand)]
        command: UsageCommand,
    },
    /// Read startup-stage timings from bounded diagnostic logs without starting an owner.
    Startup(crate::startup::StartupOptions),
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
pub enum UsageCommand {
    Get(UsageOptions),
}
#[derive(Debug, Clone, Args)]
pub struct UsageOptions {
    /// Inspect one saved attempt's usage, cache diagnostic and request byte facts.
    #[arg(long="attempt", conflicts_with_all=["thread_id","run_id","turn_id","include_children","since","until","group_by","timezone","limit","cursor"])]
    pub attempt_id: Option<String>,
    /// Return bounded saved-attempt groups instead of ungrouped totals.
    #[arg(long, value_parser = ["hour", "day", "thread"])]
    pub group_by: Option<String>,
    /// Bucket timezone: local (owner default), UTC, or a fixed +HH:MM/-HH:MM.
    #[arg(long, requires = "group_by")]
    pub timezone: Option<String>,
    /// Groups per response (1..=168; default 168).
    #[arg(long, requires = "group_by")]
    pub limit: Option<u32>,
    #[arg(long, requires = "group_by")]
    pub cursor: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long="thread", conflicts_with_all=["run_id","turn_id"])]
    pub thread_id: Option<String>,
    #[arg(long = "run", conflicts_with = "turn_id")]
    pub run_id: Option<String>,
    #[arg(long = "turn")]
    pub turn_id: Option<String>,
    #[arg(long="include-children", conflicts_with_all=["run_id","turn_id"])]
    pub include_children: bool,
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
    /// Release this Thread's hold on originals of summaries and spills adopted before a node (irreversible).
    #[command(
        after_help = "Explicit only; nothing prunes automatically. Preview first, then run:\n  shellboy thread prune --thread THREAD_ID --before NODE_ID --dry-run\n  shellboy thread prune --thread THREAD_ID --before NODE_ID\n  shellboy storage collect --category history   # optional: reclaim now and read the counts\n\nOmitting --thread targets the calling Thread. Originals still held by another live Thread (a fork) are kept. After the history GC reclaims them, a summary restore, original search and `history get` of those nodes report them as pruned. GC frees pages inside the database file but does not shrink it (no automatic VACUUM). Use `history get` to read what you need before pruning."
    )]
    Prune(ThreadPruneOptions),
    /// Inspect deletion blockers and direct references without deleting anything.
    DeletePreview(ThreadDeleteOptions),
    Set(ThreadSetOptions),
    /// Debug: assemble a saved Thread's request without sending it; read-only, no owner.
    #[command(
        after_help = "Inspect what a Thread sends, for example to find where two requests diverge:\n  shellboy thread request --thread THREAD_ID\n  shellboy thread request --thread THREAD_ID --attempt ATTEMPT_ID --compare-attempt NEXT_ATTEMPT_ID\n\nOpens the data directory's database read-only and never calls a model. Items list type, role, characters and a short hash; --body adds the body JSON. Headers and credentials are never shown; configured extra_body values appear as hashes. Parts saved state cannot determine are reported as unknown."
    )]
    Request(crate::request_inspect::RequestOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum MessageCommand {
    /// Save an instruction; acceptance is separate from execution and answer completion.
    #[command(
        after_help = "Submit and follow the returned assignment:\n  shellboy message send --thread THREAD_ID --text 'Summarize the findings'\n  shellboy message get --message MESSAGE_ID\n  shellboy turn get --turn TURN_ID\n  shellboy history get --node END_MESSAGE_NODE_ID\n\nUse only IDs returned for this request. pending has no assigned Turn yet; inspect the same message instead of sending it again. Guide: shellboy docs get --id shellboy/delegation"
    )]
    Send(MessageSendOptions),
    /// Reply once to a received reply-expected message.
    Reply(MessageReplyOptions),
    Get(MessageOptions),
    /// Cancel explicit queued messages; admitted history cannot be cancelled.
    Cancel(MessageCancelOptions),
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
    /// Inspect state and end_message_node_id; fetch the end message with history get.
    #[command(
        after_help = "  shellboy turn get --turn TURN_ID\n  shellboy history get --node END_MESSAGE_NODE_ID\n\nFetch only a returned end_message_node_id; stopped/failed terminal states need not have an end message. Guide: shellboy docs get --id shellboy/delegation"
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
    /// List saved executions across conversations without reading their output.
    List(ShellListOptions),
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
    Rebuild(IndexRebuildOptions),
    /// Send one fixed embedding input (may incur charges) and retain its result.
    Probe(IndexProbeOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum StorageCommand {
    /// Check SQLite integrity and foreign keys while the owner is stopped.
    Check,
    /// Shrink shellboy.db by rewriting it (owner stopped; explicit only). Reports sizes first.
    #[command(
        after_help = "Run with the owner stopped; refused while an owner holds the data directory.\n  shellboy storage vacuum --dry-run\n  shellboy storage vacuum\n\nReports reclaimable bytes (freelist pages x page size), file/WAL sizes and the free space VACUUM needs, and refuses if the filesystem lacks it. Never runs automatically. Run after `thread prune` and `storage collect --category history`."
    )]
    Vacuum(VacuumOptions),
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
    after_help = "Examples:\n  shellboy status\n  shellboy status --memory\n  shellboy operation get --operation OPERATION_ID\n  shellboy status --thread THREAD\n  shellboy status --run RUN --all\n  shellboy status --state failed --since 2026-10-25T00:00:00Z\n\nActivity mode reads saved database facts only. --memory observes current owner cache accounting and native process memory. Neither mode starts model or RAG work.\nUse operation get --operation ID to inspect an operation receipt.\nGuide: shellboy docs get --id shellboy/agent-guide"
)]
pub struct StatusOptions {
    /// Query the running owner's cache accounting and native process memory.
    #[arg(long, conflicts_with_all = ["thread_id", "run_id", "since", "state", "all", "limit", "cursor"])]
    pub memory: bool,
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
#[derive(Debug, Clone, Subcommand)]
pub enum CompactCommand {
    List(CompactListOptions),
}
#[derive(Debug, Clone, Args)]
pub struct CompactListOptions {
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}
#[derive(Debug, Clone, Args)]
pub struct ShellListOptions {
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long)]
    pub command_contains: Option<String>,
    #[arg(long)]
    pub state: Option<String>,
    #[arg(long, allow_negative_numbers = true)]
    pub exit_code: Option<i32>,
    #[arg(long, default_value_t = 50)]
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
#[command(group(clap::ArgGroup::new("edit_action").args(["spill", "summary", "trim", "omit", "restore", "maximal"]).multiple(false)))]
pub struct CompactOptions {
    /// Forecast selection, replay characters and failures without changing saved work or calling a model.
    #[arg(long, conflicts_with = "restore")]
    pub dry_run: bool,
    #[command(subcommand)]
    pub command: Option<CompactCommand>,
    /// Override the absolute summary limit (minimum effective limit: 2000 characters).
    #[arg(long, requires = "summary")]
    pub max_chars: Option<u64>,
    /// Override the summary limit as a percentage of the original range (0 < P <= 100).
    #[arg(long, requires = "summary")]
    pub max_percent: Option<f64>,
    /// Maximum API-free reduction outside protected work; no node IDs.
    #[arg(long)]
    pub maximal: bool,
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
    /// Instruction file; relative to the target effective cwd.
    #[arg(long)]
    pub agents: Option<PathBuf>,
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
    /// Instruction file; relative to the target effective cwd.
    #[arg(long)]
    pub agents: Option<PathBuf>,
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
pub struct ThreadPruneOptions {
    /// Target Thread; omitted means the calling Thread.
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    /// Node on the selected path; summaries and spills adopted before it are pruned.
    #[arg(long = "before")]
    pub before_node_id: String,
    /// Report what would be pruned without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}
#[derive(Debug, Clone, Args)]
pub struct ThreadSetOptions {
    /// Instruction file; relative to the target effective cwd.
    #[arg(long)]
    pub agents: Option<PathBuf>,
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
    /// Proactive compaction threshold in tokens for this Thread; 0 disables it, an empty string restores the connection value.
    #[arg(long, allow_hyphen_values = true)]
    pub auto_compact_at: Option<String>,
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
    /// Require a deliberate reply to this message.
    #[arg(long)]
    pub expect_reply: bool,
    /// Self-asserted name (at most 64 Unicode characters); grants no authority.
    #[arg(long)]
    pub sender_label: Option<String>,
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    /// Delivery timing. Omitted: the next safe boundary. now: also release a
    /// foreground shell wait (never interrupts response generation). complete:
    /// when the Thread's live Run would complete, or immediately if none is live.
    #[arg(long, value_enum)]
    pub deliver: Option<wire::Deliver>,
    #[command(flatten)]
    pub text: TextOptions,
}
#[derive(Debug, Clone, Args)]
pub struct MessageReplyOptions {
    #[arg(long = "message")]
    pub message_id: String,
    #[arg(long)]
    pub sender_label: Option<String>,
    #[command(flatten)]
    pub text: TextOptions,
}
#[derive(Debug, Clone, Args)]
pub struct MessageCancelOptions {
    #[arg(long = "message", required = true, num_args = 1, action = clap::ArgAction::Append)]
    pub message_ids: Vec<String>,
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
    after_help = "Wait for a saved target:\n  shellboy wait --conditions-json '[{\"type\":\"shell_ended\",\"execution_id\":\"WORK_EXECUTION_ID\"}]' --timeout-ms 30000\n  shellboy wait --conditions-json '[{\"type\":\"turn_completed\",\"turn_id\":\"CHILD_TURN_ID\"}]' --timeout-ms 30000\n  shellboy wait --conditions-json '[{\"type\":\"reply_received\",\"message_id\":\"MESSAGE_ID\"}]' --timeout-ms 30000\n\nThread conditions: {\"type\":\"thread_settled\",\"thread_ids\":[\"A\",\"B\"],\"states\":[\"completed\",\"recovery_wait\",\"failed\",\"stopped\"]}. Reply condition: {\"type\":\"reply_received\",\"message_id\":\"MESSAGE_ID\"}. Reuse result.cursor with --after and unchanged conditions to catch saved transitions.\n\nConditions use any semantics. timeout leaves the target running; satisfied means inspect target state, not guaranteed success. recovery_wait means inspect Run/Turn and choose recovery explicitly. Guide: shellboy docs get --id shellboy/delegation"
)]
pub struct WaitOptions {
    /// JSON array of typed thread_settled/turn_completed/run_ended/shell_ended/shell_output/reply_received conditions.
    #[arg(long)]
    pub conditions_json: String,
    /// Continue saved Thread transitions from a previous wait cursor.
    #[arg(long)]
    pub after: Option<String>,
    #[arg(long, default_value_t = wire::DEFAULT_WAIT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}
#[derive(Debug, Clone, Args)]
pub struct HistoryListOptions {
    /// Maximum Unicode scalar characters in each excerpt; zero omits its content.
    #[arg(long, default_value_t=wire::DEFAULT_HISTORY_EXCERPT_CHARS, value_parser=clap::value_parser!(u32).range(0..=wire::MAX_HISTORY_EXCERPT_CHARS as i64))]
    pub excerpt_chars: u32,
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
    /// Read only the saved end message (final assistant message); exclude reasoning and commentary.
    #[arg(long)]
    pub end_message_only: bool,
    /// For a summary node: list its saved summary parts (map then reduce) with per-part range, stage, usage and paged bodies.
    #[arg(long, conflicts_with = "end_message_only")]
    pub parts: bool,
    #[arg(long = "node")]
    pub node_id: String,
    #[arg(long,default_value_t=wire::MAX_BODY_BYTES)]
    pub limit_bytes: u32,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long, default_value = wire::DEFAULT_ENCODING)]
    pub encoding: String,
}
impl ValueEnum for wire::Deliver {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Now, Self::Complete]
    }
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.as_str()))
    }
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
#[derive(Debug, Clone, Args)]
pub struct IndexRebuildOptions {
    #[command(flatten)]
    pub selection: IndexOptions,
    #[arg(long)]
    pub dimensions: Option<usize>,
}
#[derive(Debug, Clone, Args)]
pub struct IndexProbeOptions {
    #[arg(long)]
    pub dimensions: Option<usize>,
}
impl From<IndexRebuildOptions> for wire::IndexArgs {
    fn from(value: IndexRebuildOptions) -> Self {
        Self {
            dimensions: value.dimensions,
            ..value.selection.into()
        }
    }
}
impl From<IndexOptions> for wire::IndexArgs {
    fn from(value: IndexOptions) -> Self {
        Self {
            dimensions: None,
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
pub struct VacuumOptions {
    /// Report sizes and required space without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}
#[derive(Debug, Clone, Args)]
pub struct StorageOptions {
    /// Category for collection; status always reports all categories.
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
            CliCommand::Status(a) => C::Status(wire::StatusArgs {
                memory: a.memory,
                thread_id: a.thread_id,
                run_id: a.run_id,
                since: a.since,
                state: a.state,
                all: a.all,
                limit: a.limit,
                cursor: a.cursor,
            }),
            CliCommand::Usage {
                command: UsageCommand::Get(a),
            } => C::UsageGet(wire::UsageGetArgs {
                attempt_id: a.attempt_id.map(String::into_boxed_str),
                group_by: a.group_by.map(String::into_boxed_str),
                timezone: a.timezone.map(String::into_boxed_str),
                limit: a.limit,
                cursor: a.cursor.map(String::into_boxed_str),
                since: a.since,
                until: a.until,
                thread_id: a.thread_id,
                run_id: a.run_id,
                turn_id: a.turn_id,
                include_children: a.include_children,
            }),
            CliCommand::Serve
            | CliCommand::Shutdown
            | CliCommand::Exec(_)
            | CliCommand::Docs { .. }
            | CliCommand::Startup(_)
            | CliCommand::Rag(_)
            | CliCommand::Config { .. } => {
                return Ok(None);
            }
            CliCommand::Model {
                command: ModelCommand::List(a),
            } => C::ModelList(wire::ModelListArgs {
                connection: a.connection,
                thread_id: a.thread_id,
            }),
            CliCommand::Thread { command } => match command {
                ThreadCommand::Request(_) => return Ok(None),
                ThreadCommand::List(a) => C::ThreadList(wire::ThreadListArgs {
                    limit: a.limit,
                    cursor: a.cursor,
                    query: a.query,
                }),
                ThreadCommand::Get(a) => C::ThreadGet(wire::ThreadTarget {
                    thread_id: a.thread_id,
                }),
                ThreadCommand::Create(a) => C::ThreadCreate(wire::ThreadCreateArgs {
                    agents: path_option(a.agents)?,
                    connection: a.connection,
                    cwd: path_option(a.cwd)?,
                    model: a.model,
                    effort: a.effort,
                    parent_id: a.parent_id,
                }),
                ThreadCommand::Fork(a) => C::ThreadFork(wire::ThreadForkArgs {
                    agents: path_option(a.agents)?,
                    thread_id: a.thread_id,
                    node_id: a.node_id,
                    cwd: path_option(a.cwd)?,
                }),
                ThreadCommand::Delete(a) => C::ThreadDelete(wire::ThreadDeleteArgs {
                    preview_marker: None,
                    thread_id: a.thread_id,
                    descendants: a.descendants,
                }),
                ThreadCommand::DeletePreview(a) => C::ThreadDeletePreview(wire::ThreadDeleteArgs {
                    preview_marker: None,
                    thread_id: a.thread_id,
                    descendants: a.descendants,
                }),
                ThreadCommand::Prune(a) => C::ThreadPrune(wire::ThreadPruneArgs {
                    thread_id: a.thread_id,
                    before_node_id: a.before_node_id,
                    dry_run: a.dry_run,
                }),
                ThreadCommand::Set(a) => C::ThreadSet(wire::ThreadSetArgs {
                    agents: path_option(a.agents)?,
                    title: a.title,
                    connection: a.connection,
                    thread_id: a.thread_id,
                    cwd: path_option(a.cwd)?,
                    model: a.model,
                    effort: a.effort,
                    auto_compact_at: a.auto_compact_at,
                }),
            },
            CliCommand::Message { command } => match command {
                MessageCommand::Send(a) => C::MessageSend(wire::MessageSendArgs {
                    expect_reply: a.expect_reply,
                    sender_label: a.sender_label,
                    thread_id: a.thread_id,
                    text: read_text(a.text, stdin).await?,
                    deliver: a.deliver.unwrap_or_default(),
                }),
                MessageCommand::Reply(a) => C::MessageReply(wire::MessageReplyArgs {
                    message_id: a.message_id,
                    sender_label: a.sender_label,
                    text: read_text(a.text, stdin).await?,
                }),
                MessageCommand::Get(a) => C::MessageGet(wire::MessageTarget {
                    message_id: a.message_id,
                }),
                MessageCommand::Cancel(a) => C::MessageCancel(wire::MessageCancelArgs {
                    message_ids: a.message_ids,
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
                after: a.after,
            }),
            CliCommand::History { command } => match command {
                HistoryCommand::List(a) => C::HistoryList(wire::HistoryListArgs {
                    excerpt_chars: a.excerpt_chars,
                    thread_id: a.thread_id,
                    limit: a.limit,
                    cursor: a.cursor,
                    run_id: a.run_id,
                    since: a.since,
                    state: a.state,
                }),
                HistoryCommand::Get(a) => C::HistoryGet(wire::HistoryGetArgs {
                    end_message_only: a.end_message_only,
                    parts: a.parts,
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
                ShellCommand::List(a) => C::ShellList(wire::ShellListArgs {
                    thread_id: a.thread_id,
                    since: a.since,
                    until: a.until,
                    command_contains: a.command_contains,
                    state: a.state,
                    exit_code: a.exit_code,
                    limit: a.limit,
                    cursor: a.cursor,
                }),
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
                if let Some(CompactCommand::List(list)) = a.command {
                    if a.dry_run
                        || a.spill
                        || a.summary
                        || a.trim
                        || a.omit
                        || a.restore
                        || a.maximal
                        || a.from_node_id.is_some()
                        || a.to_node_id.is_some()
                        || a.max_chars.is_some()
                        || a.max_percent.is_some()
                    {
                        return Err(wire::ApiError::new(
                            wire::ErrorCode::InvalidArgument,
                            "compact list cannot be mixed with edit flags",
                        ));
                    }
                    let cmd = C::CompactList(wire::CompactListArgs {
                        thread_id: list.thread_id.or(a.thread_id),
                        since: list.since,
                        until: list.until,
                        limit: list.limit,
                    });
                    cmd.validate()?;
                    return Ok(Some(cmd));
                }

                if (a.max_chars.is_some() || a.max_percent.is_some()) && !a.summary {
                    return Err(invalid("summary limits require --summary"));
                }
                let action = if a.maximal {
                    Some(wire::ContextEditAction::Maximal)
                } else if a.spill {
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
                if a.dry_run {
                    let cmd = C::CompactPreview(Box::new(wire::CompactPreviewArgs {
                        thread_id: a.thread_id,
                        action,
                        from_node_id: a.from_node_id,
                        to_node_id: a.to_node_id,
                        max_chars: a.max_chars,
                        max_percent: a.max_percent,
                    }));
                    cmd.validate()?;
                    return Ok(Some(cmd));
                }
                match action {
                    Some(action) => C::ContextEdit(wire::ContextEditArgs {
                        thread_id: a.thread_id,
                        action,
                        from_node_id: a.from_node_id,
                        to_node_id: a.to_node_id,
                        max_chars: a.max_chars,
                        max_percent: a.max_percent,
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
                IndexCommand::Probe(a) => C::IndexProbe(wire::IndexProbeArgs {
                    dimensions: a.dimensions,
                }),
            },
            CliCommand::Storage { command } => match command {
                StorageCommand::Check => {
                    return Err(invalid("storage check is a standalone command"));
                }
                StorageCommand::Vacuum(_) => {
                    return Err(invalid("storage vacuum is a standalone command"));
                }
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
    let max_bytes = MAX_STDIN_TEXT_BYTES;
    match (options.text, options.text_stdin) {
        (Some(text), false) => {
            if text.len() > max_bytes {
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
                .take((max_bytes + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| ApiError::new(ErrorCode::IoFailed, "cannot read text from stdin"))?;
            if bytes.len() > max_bytes {
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
