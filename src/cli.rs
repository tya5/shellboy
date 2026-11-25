//! Human/model CLI syntax. Conversion produces the same typed operations as TUI.
use crate::protocol::{self as wire, ApiError, ErrorCode};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{
    future::Future,
    path::{Component, Path, PathBuf},
};
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
    /// Inspect versioned RPC contracts and generated schemas.
    Protocol {
        #[command(subcommand)]
        command: ProtocolCommand,
    },
    /// Ask a terminal-free owner to shut down; requires an explicit --data-dir.
    Shutdown,
    /// Talk to a conversation line by line through a running owner; see docs for input and exit rules.
    Chat(crate::chat_client::ChatOptions),
    /// Run saved work noninteractively; validate only its final JSON answer locally.
    Exec(crate::exec_client::ExecOptions),
    /// Inspect recorded work; no model call or persistent job engine.
    Status(StatusOptions),
    /// Create or check a configuration without an owner; read or change it through one.
    Config {
        #[command(subcommand)]
        command: ConfigCliCommand,
    },
    /// Store or remove an API key for a configured connection; needs a running owner.
    Credentials {
        #[command(subcommand)]
        command: CredentialsCommand,
    },
    /// Check a connection by asking its endpoint for the model catalog; needs a running owner.
    Connection {
        #[command(subcommand)]
        command: ConnectionCommand,
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
    /// Wait for saved-state change hints (a cursor over per-Thread change order).
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
    /// Inspect original history and search the active embedding space.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Manage versioned artifact metadata owned by a Thread.
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
    /// Manage long-running shell services owned by a Thread.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Manage scheduled commands owned by a Thread.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
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
    /// Attach a path; shorthand for attachment add --path PATH --kind KIND (image or file).
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
pub enum ProtocolCommand {
    /// Write the current protocol JSON Schema to standard output.
    Schema,
}

#[derive(Debug, Clone, Subcommand)]
pub enum CredentialsCommand {
    /// Store the API key read from standard input for a connection (never from an argument).
    #[command(
        after_help = "Needs a running owner; --config does not select the file.\n  printf %s \"$KEY\" | shellboy credentials set --connection proxy\n\nThe key is read from standard input only, so it never appears in the process list or shell history; a terminal is refused so the key is not echoed. It is stored as plain text in credentials.toml beside config.toml (mode 0600 on Unix; on Windows the user profile's permissions apply). An environment variable named by api_key_env takes precedence. The reply never contains the key."
    )]
    Set(CredentialsOptions),
    /// Remove the stored key of a connection.
    Clear(CredentialsOptions),
}
#[derive(Debug, Clone, Args)]
pub struct CredentialsOptions {
    /// Connection name from the configuration file.
    #[arg(long)]
    pub connection: String,
}
#[derive(Debug, Clone, Subcommand)]
pub enum ConnectionCommand {
    /// Test the model catalog; a conversation POST requires explicit opt-in.
    #[command(
        after_help = "Needs a running owner; --config does not select the file.\n  shellboy connection test --connection proxy\n  printf %s \"$KEY\" | shellboy connection test --base-url https://api.example/v1 --api-key-stdin\n  shellboy connection test --base-url http://localhost:8080/v1 --api-key-env LOCAL_KEY\n\nReports http_status, the model IDs, whether api_key_env is set in the owner's environment, and a failure class (auth, url, unreachable, timeout, ...). By default only the catalog is tested. --post-model MODEL additionally sends one paid synthetic conversation request."
    )]
    Test(ConnectionTestOptions),
}
#[derive(Debug, Clone, Args)]
pub struct ConnectionTestOptions {
    /// A saved connection; otherwise describe a candidate with --base-url.
    #[arg(long, conflicts_with_all = ["base_url", "api", "api_key_stdin", "api_key_env", "api_key_header", "no_ssl_verify"])]
    pub connection: Option<String>,
    /// API root of a candidate that is not saved, e.g. https://proxy.example/v1.
    #[arg(long)]
    pub base_url: Option<String>,
    /// Dialect of the candidate: responses (default), chat_completions or anthropic_messages.
    #[arg(long)]
    pub api: Option<String>,
    /// Read the candidate's key from standard input (never from an argument).
    #[arg(long, conflicts_with = "api_key_env")]
    pub api_key_stdin: bool,
    /// Environment variable that holds the candidate's key in the owner's environment.
    #[arg(long)]
    pub api_key_env: Option<String>,
    /// Header that carries the key instead of Authorization: Bearer.
    #[arg(long)]
    pub api_key_header: Option<String>,
    /// Do not verify the candidate's HTTPS certificate.
    #[arg(long)]
    pub no_ssl_verify: bool,
    /// Also send one paid synthetic conversation POST to this model; never retried.
    #[arg(long)]
    pub post_model: Option<String>,
}
const MAX_STDIN_KEY_BYTES: usize = 8 * 1024;
/// One line from standard input; a single trailing line break is dropped.
async fn read_key<R: AsyncRead + Unpin>(reader: &mut R) -> Result<String, ApiError> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_STDIN_KEY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "cannot read the key from stdin"))?;
    if bytes.len() > MAX_STDIN_KEY_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "the key on stdin is too long",
        ));
    }
    let mut key =
        String::from_utf8(bytes).map_err(|_| invalid("the key on stdin must be valid UTF-8"))?;
    if key.ends_with('\n') {
        key.pop();
        if key.ends_with('\r') {
            key.pop();
        }
    }
    Ok(key)
}
#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCliCommand {
    #[command(flatten)]
    Setup(crate::config_setup::ConfigCommand),
    /// Read what the running owner loaded and the current file; credential values are never returned.
    #[command(
        after_help = "Needs a running owner; --config does not select the file. Use the returned file.content_hash as --expected-hash for config set."
    )]
    Get,
    /// Change named configuration fields; connections/defaults apply live, except the embedding connection.
    #[command(
        after_help = "Needs a running owner; --config does not select the file.\n  shellboy config set --expected-hash HASH --set default_model=my-model --set ui.bell=false\n\nFIELD is a TOML dotted key, e.g. connections.proxy.base_url or connections.\"my.proxy\".base_url. For a dotted connection name, preserve the TOML quotes: --set 'connections.\"my.proxy\".base_url=https://example.test/v1'. VALUE is JSON when it parses as JSON and a plain string otherwise; write '\"123\"' for the string 123. Credentials stay in the environment: api_key_env takes a variable name."
    )]
    Set(ConfigSetOptions),
}
#[derive(Debug, Parser)]
#[command(name = "/config", disable_help_subcommand = true)]
struct ConfigSlash {
    #[command(subcommand)]
    verb: ConfigSlashVerb,
}

#[derive(Debug, Subcommand)]
enum ConfigSlashVerb {
    /// Read the running owner's loaded and current settings.
    Get,
    /// Update the running owner's settings with the current content hash.
    Set(ConfigSetOptions),
    /// Check configuration using the active session's paths.
    Check,
}

pub(crate) enum ConfigSlashAction {
    Owner(Box<wire::Command>),
    Check,
}

pub(crate) fn config_slash(text: &str) -> Result<ConfigSlashAction, String> {
    let words = shlex::split(text).ok_or_else(|| "Unclosed quote or escape".to_owned())?;
    let parsed = ConfigSlash::try_parse_from(
        std::iter::once("/config").chain(words.iter().map(String::as_str)),
    )
    .map_err(|error| error.to_string())?;
    let command = match parsed.verb {
        ConfigSlashVerb::Get => wire::Command::ConfigGet(wire::ConfigGetArgs {}),
        ConfigSlashVerb::Set(options) => {
            wire::Command::ConfigSet(options.into_args().map_err(|error| error.message)?)
        }
        ConfigSlashVerb::Check => return Ok(ConfigSlashAction::Check),
    };
    command.validate().map_err(|error| error.message)?;
    Ok(ConfigSlashAction::Owner(Box::new(command)))
}

#[derive(Debug, Clone, Args)]
pub struct ConfigSetOptions {
    /// file.content_hash from config get; refused if the file has changed since.
    #[arg(long)]
    pub expected_hash: String,
    /// FIELD=VALUE; repeatable.
    #[arg(long = "set", value_name = "FIELD=VALUE")]
    pub set: Vec<String>,
    /// Remove FIELD so its default applies; repeatable.
    #[arg(long = "unset", value_name = "FIELD")]
    pub unset: Vec<String>,
}
impl ConfigSetOptions {
    fn into_args(self) -> Result<wire::ConfigSetArgs, ApiError> {
        let mut changes = Vec::with_capacity(self.set.len() + self.unset.len());
        for item in self.set {
            let (field, text) = item
                .split_once('=')
                .ok_or_else(|| invalid("--set takes FIELD=VALUE"))?;
            let value = serde_json::from_str(text)
                .unwrap_or_else(|_| serde_json::Value::String(text.to_owned()));
            if value.is_null() {
                return Err(invalid(
                    "--set takes a value; use --unset FIELD to remove one",
                ));
            }
            changes.push(wire::ConfigChange {
                field: field.to_owned(),
                value: Some(value),
            });
        }
        changes.extend(
            self.unset
                .into_iter()
                .map(|field| wire::ConfigChange { field, value: None }),
        );
        Ok(wire::ConfigSetArgs {
            expected_hash: self.expected_hash,
            changes,
        })
    }
}
#[derive(Debug, Clone, Subcommand)]
pub enum EventsCommand {
    /// Long-poll: return which Threads changed after CURSOR, or time out. Re-read saved facts afterwards.
    Wait(EventsWaitOptions),
}
#[derive(Debug, Clone, Args)]
#[command(
    after_help = "Without --after the call returns a baseline cursor at once. Reuse result.cursor with --after. status=resync_required means re-read everything you show (owner restarted or the cursor fell out of the bounded window). Changes are hints that coalesce; read saved facts with thread get, history list, shell list, turn get. Up to 64 --thread filters. Clients must use an owner advertising the same protocol version. --text also returns unsaved answer text in live_text; reasoning content is excluded. --reasoning-activity returns active attempts without model text (status=text when an activity observation is new): a lossy preview for UI clients, never saved. reasoning_activity.elapsed_ms is local attempt elapsed time; received_bytes is observed UTF-8 reasoning bytes, not tokens, and is omitted while unknown. It never contains reasoning content. On gap=true, a new attempt_id or an attempt no longer listed, discard what you showed. Saved output is what stays."
)]
pub struct EventsWaitOptions {
    /// Cursor from a previous events wait; omit for a baseline.
    #[arg(long)]
    pub after: Option<String>,
    /// Only report this Thread; repeat for up to 64 Threads.
    #[arg(long = "thread")]
    pub thread_ids: Vec<String>,
    #[arg(long, default_value_t = wire::DEFAULT_WAIT_TIMEOUT_MS)]
    pub timeout_ms: u64,
    /// Also return unsaved non-reasoning answer text being written now (a lossy preview for UI clients).
    #[arg(long)]
    pub text: bool,
    /// Return bounded active-attempt metadata, never model text.
    #[arg(long)]
    pub reasoning_activity: bool,
}
#[derive(Debug, Clone, Subcommand)]
pub enum UsageCommand {
    Get(UsageOptions),
}
#[derive(Debug, Clone, Args)]
pub struct UsageOptions {
    /// Inspect one saved attempt's usage, cache diagnostic and request byte facts.
    #[arg(long="attempt", conflicts_with_all=["thread_id","run_id","turn_id","include_children","since","since_node_id","until","group_by","timezone","limit","cursor"])]
    pub attempt_id: Option<String>,
    /// Return bounded saved-attempt groups instead of ungrouped totals.
    #[arg(long, value_parser = ["hour", "day", "thread", "model", "shell-exit"])]
    pub group_by: Option<String>,
    /// Bucket timezone: local (owner default), UTC, or a fixed +HH:MM/-HH:MM.
    #[arg(long, requires = "group_by")]
    pub timezone: Option<String>,
    /// Groups per response (1..=168; default 168).
    #[arg(long, requires = "group_by")]
    pub limit: Option<u32>,
    #[arg(long, requires = "group_by")]
    pub cursor: Option<String>,
    #[arg(long, conflicts_with = "since_node_id")]
    pub since: Option<String>,
    /// Start at this saved node; mutually exclusive with --since.
    #[arg(long = "since-node", conflicts_with = "since")]
    pub since_node_id: Option<String>,
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
    /// Save retained conversation history as a JSONL file.
    Export(crate::thread_export::ExportOptions),
    /// Create a new Thread from a verified JSONL export.
    Import {
        #[arg(long = "in")]
        input: PathBuf,
    },
    /// Create a saved Thread; send a message separately to start work.
    #[command(
        after_help = "Delegate from a parent model shell:\n  shellboy thread create --parent-thread THREAD_ID\n  shellboy message send --thread CHILD_THREAD_ID --text 'Read-only audit; return findings and evidence'\n\nRead result.thread_id from create, then track the separate message_id/run_id/turn_id from send. Guide: shellboy docs get --id shellboy/delegation"
    )]
    Create(ThreadCreateOptions),
    Fork(ThreadForkOptions),
    Delete(ThreadDeleteOptions),
    /// Release this Thread's hold on originals of summaries and spills adopted before a node (irreversible).
    #[command(
        after_help = "Explicit only; nothing prunes automatically. Preview first, then run:\n  shellboy thread prune --thread THREAD_ID --before NODE_ID --dry-run\n  shellboy thread prune --thread THREAD_ID --before NODE_ID\n  shellboy storage collect --category history   # optional: reclaim now and read the counts\n\nOmitting --thread targets the calling Thread. Originals still held by another live Thread (a fork) are kept. After the history GC reclaims them, a summary restore, original search and `history read` of those nodes report them as pruned. GC frees pages inside the database file but does not shrink it (no automatic VACUUM). Use `history read` to read what you need before pruning."
    )]
    Prune(ThreadPruneOptions),
    /// Cut this Thread's selected path back to a node; the removed part stays as a child Thread.
    #[command(
        after_help = "Same Thread, same ID and title; the path now ends at NODE:\n  shellboy thread rewind --thread THREAD_ID --node NODE_ID\n\nThe removed part is kept as a new child Thread (branch_thread_id in the result); opening it undoes the rewind. There is no in-place forward. NODE is a node on the selected path, or an original inside a summary this Thread holds (not after `thread prune`). Conversation only: shell executions and child Threads started after NODE are listed, not undone. Refused while the Thread has a live Run or messages with unknown path admission, so a model cannot rewind the Thread it is running in; it can rewind another idle Thread. `--run RUN_ID` instead atomically stops that exact recovery_wait Run and rewinds, refusing retained owners without cancelling them. It requires `--thread`; no Run is inferred. Attachments whose message leaves the path are not sent. `--dry-run` reports the same facts (removed range, what is not undone) without changing anything."
    )]
    Rewind(ThreadRewindOptions),
    /// Show the branch tree: Threads forked, edited or rewound from this one, and where they diverge.
    #[command(
        after_help = "A branch is a child Thread that shares a path prefix with its parent (fork, /edit, `thread rewind`). Read-only and bounded:\n  shellboy thread branches [--thread THREAD_ID] [--limit N] [--cursor TOKEN]\n\nItems are the Threads of the family of THREAD_ID (its topmost live ancestor and everything below), ancestors first then breadth first. Each item has: parent_thread_id, state (current, ancestor, branch), running, pruned_originals, divergence (the last node shared with the parent, its request, and whether the parent continues), tip (last request, time), size (path_nodes; bytes is null: not summed). Bounds: 256 Threads per family (family_truncated), 32 ancestors, 256 path rows searched for a last request, 160 excerpt characters, 1..=200 items per page. Open a branch with `shellboy thread get` or in the TUI (/switch ID); delete or prune it with `thread delete` / `thread prune`."
    )]
    Branches(ThreadBranchesOptions),
    /// Inspect deletion blockers and direct references without deleting anything.
    DeletePreview(ThreadDeleteOptions),
    /// Hide Threads from the default list without deleting anything; reversible.
    #[command(
        after_help = "Archive is organization only: history, search, usage and receiving messages are unchanged, and it frees no storage (`thread delete` is separate). Hidden from `thread list` unless `--archived include|only`. A new Run on an archived Thread unarchives it. Nothing is archived automatically.\n  shellboy thread archive --thread ID [--thread ID ...] [--descendants]\n\nUp to 200 explicit IDs; `--descendants` adds the current subtree (a selection over 256 Threads is refused with input_limit). Omitting --thread archives the calling Thread. To archive many finished Threads, list first, then pass the IDs:\n  shellboy thread list --limit 200\n  shellboy thread archive --thread ID1 --thread ID2\n\nAllowed while a Run is live (it keeps running; `status` and Watch still show it). Does not change updated_at, so list order does not move. Result: archived, already, not_found."
    )]
    Archive(ThreadArchiveOptions),
    /// Return archived Threads to the default list.
    #[command(
        after_help = "  shellboy thread unarchive --thread ID [--thread ID ...] [--descendants]\n\nSame selection rules as `thread archive`. Result: unarchived, already, not_found. Find archived Threads with `shellboy thread list --archived only`."
    )]
    Unarchive(ThreadArchiveOptions),
    /// Add or remove tags on Threads: free labels for placement and cross-cutting views.
    #[command(
        after_help = "Tags are organization only: history, search, usage and receiving messages are unchanged, and they never change updated_at. A Thread may carry at most 64 tags; a tag is one line of at most 128 characters without control characters (surrounding spaces are trimmed; case matters). Tags are not inherited by create, fork or rewind.\n  shellboy thread tag --thread ID [--thread ID ...] --add TAG [--add TAG ...] [--remove TAG ...]\n\nUp to 200 explicit IDs and up to 64 tags to add and to remove per call. Omitting --thread tags the calling Thread. Result: changed (Threads whose tags changed), unchanged, not_found. Find Threads with `shellboy thread list --tag TAG` (repeat for AND) and the names in use with `shellboy thread tags`. To archive everything with a tag, list first, then pass the IDs to `thread archive`."
    )]
    Tag(ThreadTagOptions),
    /// List the distinct tag names in use, in byte order (names only, no counts).
    #[command(
        after_help = "  shellboy thread tags [--limit N] [--cursor CURSOR]\n\nPages of at most 200 names; pass next_cursor back as --cursor. Tags on deleted Threads are not listed."
    )]
    Tags(ThreadTagsOptions),
    /// List the distinct saved working directories of Threads, in byte order (paths only, no counts).
    #[command(
        after_help = "  shellboy thread cwds [--limit N] [--cursor CURSOR]\n\nPages of at most 200 paths; pass next_cursor back as --cursor. Every non-deleted Thread counts, archived ones included, so a directory used only by archived Threads is listed. List its Threads with `shellboy thread list --cwd PATH [--archived include|only]`."
    )]
    Cwds(ThreadTagsOptions),
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
        after_help = "Submit and follow the returned assignment:\n  shellboy message send --thread THREAD_ID --text 'Summarize the findings'\n  shellboy message get --message MESSAGE_ID\n  shellboy turn get --turn TURN_ID\n  shellboy history read --node END_MESSAGE_NODE_ID\n\nUse only IDs returned for this request. pending has no assigned Turn yet; inspect the same message instead of sending it again. Guide: shellboy docs get --id shellboy/delegation"
    )]
    Send(MessageSendOptions),
    /// Request a reply from a named recipient or another Thread.
    #[command(
        after_help = "  shellboy message ask --thread CHILD_THREAD_ID --text 'Report what you verified'\n  shellboy message ask --to human --text 'Choose A or B'\n  shellboy message ask --to claude --text 'Review the proposed change'\n  shellboy message get --message MESSAGE_ID\n\nAcceptance opens one reply duty. It does not prove delivery, execution, or an answer. Check delivery_status on the same message, inspect assigned Run and Turn IDs when present, and use reply.state and reply.node_id to identify the actual reply. A cancelled or otherwise closed duty without a reply is not an answer. Named recipients require model origin; thread_id identifies the asking Thread. Guide: shellboy docs get --id shellboy/delegation"
    )]
    Ask(MessageAskOptions),
    /// List pending replies without marking them read.
    List(MessageListOptions),
    /// Reply once to a received reply-expected message.
    Reply(MessageReplyOptions),
    /// Inspect saved delivery facts; read the original body separately with history read.
    #[command(
        after_help = "  shellboy message get --message MESSAGE_ID\n  shellboy history read --node MESSAGE_ID\n\nInspect delivery_status and existing context.hold_reason. terminal_before_path_admission is a held reason: the assigned Run/Turn ended before path admission, not proof of receipt termination or body loss. Saved context is not live delivery proof; history read reads retained body without resending it. A missing body is unknown/unavailable, not permission to invent it.\nA rewind does not by itself prove these inputs were lost. Explicit human resume can admit eligible old held inputs, so a fresh manual resend can duplicate their instructions. Do not automatically repair, resend or create a new sender notice.\nGuide: shellboy docs get --id shellboy/recovery-and-storage"
    )]
    Get(MessageOptions),
    /// Cancel queued delivery or withdraw your pending reply request after delivery. Admitted history is retained.
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
        after_help = "Inspect first: shellboy run get --run RUN_ID\nThe response acknowledges acceptance, not execution success. Follow its exact run_id/turn_id, not the Thread's latest Turn:\n  shellboy wait --conditions-json '[{\"type\":\"turn_completed\",\"turn_id\":\"ACK_TURN_ID\"}]' --timeout-ms 30000\n  shellboy turn get --turn ACK_TURN_ID\nInspect snapshot.state, context.recovery_reason and end_message_node_id. satisfied is not necessarily success; recovery_wait requires another explicit decision. Timeout/interrupted observation does not stop the target. No attempt ID exists until model preparation.\nFor repeated same-reason recovery, capture a thread_settled cursor BEFORE retry; filter changes by the acknowledged Run/Turn. Without that baseline, or after cursor expiry, the retry epoch is unknown.\nRemaining held inputs can keep recovery_wait while a bounded 128-input unit progresses without a model call. Replaying the same operation ID returns the saved acknowledgement, not the next unit.\nGuide: shellboy docs get --id shellboy/recovery-and-storage\nAny caller may retry; act only on Runs you started or were asked to handle. Terminal Runs are not revived; do not repeat an unconfirmed side effect under a new ID."
    )]
    Retry(RunOptions),
    /// Request stopping this Run and its owned work; inspect state to confirm termination.
    Stop(RunOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum TurnCommand {
    /// Inspect state and end_message_node_id; fetch the end message with history read.
    #[command(
        after_help = "  shellboy turn get --turn TURN_ID\n  shellboy history read --node END_MESSAGE_NODE_ID\n\nFetch only a returned end_message_node_id; stopped/failed terminal states need not have an end message. Guide: shellboy docs get --id shellboy/delegation"
    )]
    Get(TurnOptions),
    /// Interrupt this Turn without stopping the Run.
    Interrupt(TurnOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum HistoryCommand {
    List(HistoryListOptions),
    /// Read the selected path around an end, position or node: bounded, no cursor.
    #[command(
        after_help = "  shellboy history window --thread THREAD_ID --before 20\n  shellboy history window --thread THREAD_ID --ordinal 4000 --before 10 --after 10\n\nWith no --ordinal/--node the window ends at the last node. Each item carries selection.ordinal; total is one past the last ordinal and revision changes on append, rewind and compaction. An append keeps every earlier position valid; a stale position fails with cursor_expired (details: revision, total). Pass --expect-node with --ordinal to detect a rewound or compacted path."
    )]
    Window(HistoryWindowOptions),
    Read(HistoryReadOptions),
    Search(SearchOptions),
    Relations(HistoryRelationsOptions),
}
#[derive(clap::Subcommand, Debug, Clone)]
pub enum ArtifactCommand {
    /// Add artifact metadata.
    Add {
        #[arg(long = "thread")]
        thread_id: String,
        #[arg(long)]
        uri: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        mime: Option<String>,
    },
    /// Update artifact metadata.
    Update {
        #[arg(long = "artifact")]
        artifact_id: String,
        #[arg(long)]
        uri: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        mime: Option<String>,
    },
    /// List artifact metadata.
    List {
        #[arg(long = "thread")]
        thread_id: String,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t=wire::ARTIFACT_PAGE_LIMIT)]
        limit: u32,
    },
    /// Get artifact metadata.
    Get {
        #[arg(long = "artifact")]
        artifact_id: String,
        #[arg(id="artifact_version", long="version", conflicts_with_all=["limit", "cursor"])]
        version: Option<i64>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Remove artifact metadata.
    Remove {
        #[arg(long = "artifact")]
        artifact_id: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ServiceCommand {
    /// Save and immediately start a service.
    Add {
        #[arg(long = "thread")]
        thread_id: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        command: String,
    },
    List {
        #[arg(long = "thread")]
        thread_id: Option<String>,
    },
    Start {
        #[arg(long = "service")]
        service_id: String,
    },
    Stop {
        #[arg(long = "service")]
        service_id: String,
    },
    Remove {
        #[arg(long = "service")]
        service_id: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ScheduleCommand {
    /// Add a scheduled shell command.
    Add(ScheduleAddOptions),
    Set {
        #[arg(long = "schedule")]
        schedule_id: String,
        #[arg(long)]
        description: String,
    },
    Enable {
        #[arg(long = "schedule")]
        schedule_id: String,
    },
    Disable {
        #[arg(long = "schedule")]
        schedule_id: String,
    },
    Remove {
        #[arg(long = "schedule")]
        schedule_id: String,
    },

    /// List schedules and their latest saved results.
    List {
        #[arg(long = "thread")]
        thread_id: Option<String>,
    },
}

#[derive(Debug, Clone, Args)]
#[command(group(clap::ArgGroup::new("when").required(true).args(["cron", "at", "delay"])))]
pub struct ScheduleAddOptions {
    #[arg(long = "thread")]
    thread_id: Option<String>,
    #[arg(long)]
    cron: Option<String>,
    #[arg(long)]
    at: Option<String>,
    #[arg(long = "in", value_parser=parse_schedule_delay)]
    delay: Option<u64>,
    #[arg(long)]
    cwd: Option<String>,
    #[arg(long)]
    description: Option<String>,
    #[arg(last = true, required = true, num_args = 1..)]
    command: Vec<String>,
}

impl From<ServiceCommand> for wire::Command {
    fn from(command: ServiceCommand) -> Self {
        match command {
            ServiceCommand::Add {
                thread_id,
                cwd,
                description,
                command,
            } => Self::ServiceAdd(wire::ServiceAddArgs {
                thread_id,
                cwd,
                description,
                command,
            }),
            ServiceCommand::List { thread_id } => {
                Self::ServiceList(wire::ThreadTarget { thread_id })
            }
            ServiceCommand::Start { service_id } => {
                Self::ServiceStart(wire::ServiceTarget { service_id })
            }
            ServiceCommand::Stop { service_id } => {
                Self::ServiceStop(wire::ServiceTarget { service_id })
            }
            ServiceCommand::Remove { service_id } => {
                Self::ServiceRemove(wire::ServiceTarget { service_id })
            }
        }
    }
}

impl From<ScheduleCommand> for wire::Command {
    fn from(command: ScheduleCommand) -> Self {
        match command {
            ScheduleCommand::Add(a) => Self::ScheduleAdd(wire::ScheduleAddArgs {
                thread_id: a.thread_id,
                cron: a.cron,
                at: a.at,
                in_seconds: a.delay,
                cwd: a.cwd,
                description: a.description,
                command: a.command.join(" "),
            }),
            ScheduleCommand::Set {
                schedule_id,
                description,
            } => Self::ScheduleSet(wire::ScheduleSetArgs {
                schedule_id,
                description,
            }),
            ScheduleCommand::Enable { schedule_id } => {
                Self::ScheduleEnable(wire::ScheduleTarget { schedule_id })
            }
            ScheduleCommand::Disable { schedule_id } => {
                Self::ScheduleDisable(wire::ScheduleTarget { schedule_id })
            }
            ScheduleCommand::Remove { schedule_id } => {
                Self::ScheduleRemove(wire::ScheduleTarget { schedule_id })
            }
            ScheduleCommand::List { thread_id } => {
                Self::ScheduleList(wire::ThreadTarget { thread_id })
            }
        }
    }
}

#[derive(Parser)]
#[command(name = "", disable_help_subcommand = true)]
struct ScheduledWorkSlash {
    #[command(subcommand)]
    command: ScheduledWorkCommand,
}

#[derive(Subcommand)]
enum ScheduledWorkCommand {
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
}

pub(crate) fn scheduled_work_slash(text: &str) -> Result<wire::Command, String> {
    // Tokenize only; no shell, expansion, or execution takes place here.
    let words = shlex::split(text).ok_or_else(|| "Unclosed quote or escape".to_owned())?;
    let parsed = ScheduledWorkSlash::try_parse_from(
        std::iter::once("/").chain(words.iter().map(String::as_str)),
    )
    .map_err(|error| error.to_string())?;
    Ok(match parsed.command {
        ScheduledWorkCommand::Schedule { command } => command.into(),
        ScheduledWorkCommand::Service { command } => command.into(),
    })
}

fn parse_schedule_delay(input: &str) -> Result<u64, String> {
    let value = input.trim_end_matches(['s', 'm', 'h', 'd']);
    let scale = match &input[value.len()..] {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => 0,
    };
    value
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(scale))
        .filter(|v| *v > 0)
        .ok_or_else(|| "Specify a positive duration, such as 10m".into())
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
    /// Release the foreground wait without stopping the shell.
    Background(ExecutionOptions),
    Stop(ExecutionOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum AttachmentCommand {
    Add(AttachmentAddOptions),
    List(AttachmentListOptions),
    Remove(AttachmentRemoveOptions),
}
#[derive(Debug, Clone, Subcommand)]
pub enum IndexCommand {
    /// Job counts, spaces and pause state; takes no --kind.
    Status(IndexTargetOptions),
    /// Inspect candidate counts without embedding requests or starting jobs.
    Preview(IndexOptions),
    /// Start paid indexing of explicitly selected history. Check preview first.
    Backfill(IndexOptions),
    /// Resume failed work that was already authorized; takes no --kind.
    Retry(IndexTargetOptions),
    /// Build a candidate index; embedding API costs may apply. Check preview first.
    Rebuild(IndexRebuildOptions),
    /// Send one fixed embedding input (may incur charges) and retain its result.
    Probe(IndexProbeOptions),
    /// Shrink the active index to a quarter of its vector bytes without any API call.
    #[command(
        after_help = "Explicit only; nothing converts automatically. Preview first, then run:\n  shellboy index shrink --dry-run\n  shellboy index shrink\n  shellboy index status   # progress in `shrink`, result in `spaces`\n\nConverts the stored vectors of the active index to 8-bit values (int8) with a scale taken from those vectors. No embedding API is called and no history is read. Search keeps using the old index until the new one is complete; a failure or `--cancel` leaves the old one in place, and running `index shrink` again resumes. Afterwards rank order changes slightly, `distance` has a different scale, and new embeddings and search queries use the same fixed scale (larger values saturate).\n\nShrink only converts and never deletes: the old index stays, counts as logical data, and frees no disk until you remove it with `index remove`. `--revert [--dry-run]` switches back to it and re-queues segments indexed since through normal indexing. Unlike `index rebuild`, it costs nothing and keeps the dimension. Guide: shellboy docs get --id shellboy/history-and-search"
    )]
    Shrink(IndexShrinkOptions),
    /// Delete one inactive index for good; recreating it means paid embedding.
    #[command(
        after_help = "Preview first; only the named space is deleted:\n  shellboy index remove --space SPACE_ID --dry-run\n  shellboy index remove --space SPACE_ID\n\nFind retained spaces in `index status` (`retained_remove_with`). Refused for the active index and for a space a conversion, rebuild or running request is using. Conversation history is not touched. The database file shrinks only with `storage vacuum`."
    )]
    Remove(IndexRemoveOptions),
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
    /// Sort by updated (default) or created (newest first; only with archive selection, not query, tag, cwd, awaiting, or state filters).
    /// Created pages are live: refresh page 1 for Threads above the cursor. A new Thread below the cursor, for example after a clock rollback, can appear once on a later page.
    #[arg(long, value_enum, default_value = "updated")]
    pub order: wire::ThreadListOrder,
    #[arg(long,default_value_t=wire::DEFAULT_LIST_LIMIT)]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub query: Option<String>,
    /// Only Threads with an unanswered question for this recipient name; cannot be combined with --query, --tag, --cwd, or --state.
    #[arg(long)]
    pub awaiting: Option<String>,
    /// Only Threads whose current list state matches (updated order only).
    #[arg(long, value_enum)]
    pub state: Option<wire::ThreadListState>,
    /// Archived Threads: exclude (default, hidden), include, or only.
    #[arg(long, value_enum, default_value_t = wire::ArchivedFilter::Exclude)]
    pub archived: wire::ArchivedFilter,
    /// Only Threads carrying this tag; repeat for AND (at most 64). Follow the cursor even if a filtered page is empty.
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// Only Threads whose saved working directory is this path, compared after lexical cleanup (`.`, `..`, trailing `/`); a parent directory does not match. Relative paths resolve against the current directory.
    #[arg(long)]
    pub cwd: Option<PathBuf>,
}
#[derive(Debug, Clone, Args)]
pub struct ThreadTagOptions {
    /// Thread to change; repeat for up to 200. Omitted means the calling Thread.
    #[arg(long = "thread")]
    pub thread_ids: Vec<String>,
    /// Tag to add; repeat for up to 64.
    #[arg(long = "add")]
    pub add: Vec<String>,
    /// Tag to remove; repeat for up to 64.
    #[arg(long = "remove")]
    pub remove: Vec<String>,
}
#[derive(Debug, Clone, Args)]
pub struct ThreadTagsOptions {
    #[arg(long,default_value_t=wire::DEFAULT_LIST_LIMIT)]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, Args)]
pub struct ThreadArchiveOptions {
    /// Thread to archive; repeat for up to 200. Omitted means the calling Thread.
    #[arg(long = "thread")]
    pub thread_ids: Vec<String>,
    /// Also select the Thread's descendants (at most 256 Threads in all; larger selections are refused).
    #[arg(long)]
    pub descendants: bool,
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
    /// Filter work to this exact Run.
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
    #[arg(long, default_value_t = wire::DEFAULT_LIST_LIMIT)]
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
pub struct AttachmentListOptions {
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    #[arg(long, default_value_t = wire::DEFAULT_LIST_LIMIT)]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, Args)]
#[command(group(clap::ArgGroup::new("edit_action").args(["spill", "summary", "trim", "omit", "restore", "maximal"]).multiple(false)))]
pub struct CompactOptions {
    /// Forecast selection, replay characters and failures without changing saved work or calling a model.
    #[arg(long, conflicts_with = "restore")]
    pub dry_run: bool,
    #[command(subcommand)]
    pub command: Option<CompactCommand>,
    /// Override the requested summary output character limit.
    #[arg(long, requires = "summary")]
    pub max_chars: Option<u64>,
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
    #[arg(long = "parent-thread")]
    pub parent_thread_id: Option<String>,
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
pub struct ThreadBranchesOptions {
    /// Target Thread; omitted means the calling Thread.
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    #[arg(long, default_value_t = wire::DEFAULT_LIST_LIMIT)]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, Args)]
pub struct ThreadRewindOptions {
    /// Target Thread; omitted means the calling Thread (refused while it is running).
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    /// The node that becomes the end of the selected path.
    #[arg(long = "node")]
    pub node_id: String,
    /// Do not keep NODE: the node just before it becomes the end (rewind to before a request).
    #[arg(long)]
    pub exclude_node: bool,
    /// Report what the rewind would remove and not undo, without changing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Atomically stop this exact recovery_wait Run and rewind its Thread.
    #[arg(long = "run")]
    pub run_id: Option<String>,
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
    /// Target percentage (1-100) of the proactive threshold that decides whether the previous compaction was deep enough; an empty string restores the connection or default value.
    #[arg(long, allow_hyphen_values = true)]
    pub auto_compact_target_percent: Option<String>,
    /// Maximum characters requested for one summary (positive integer); an empty string restores the connection or configured value.
    #[arg(long, allow_hyphen_values = true)]
    pub summary_max_chars: Option<String>,
    /// Model output limit (tokens, positive integer) for this Thread's requests, replacing the connection max_tokens; an empty string restores the connection or default value.
    #[arg(long, allow_hyphen_values = true)]
    pub max_tokens: Option<String>,
    /// Automatic spill policy for this Thread; on/off, with an empty string to inherit.
    #[arg(long)]
    pub spill: Option<String>,
    /// Move this Thread under another live Thread; an empty string detaches it (no parent). Live Runs keep their owner.
    #[arg(long = "parent-thread")]
    pub parent_thread_id: Option<String>,
}
#[derive(Debug, Clone, Default, Args)]
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
    /// Maximum model requests for a new Run; refuse if the Thread already has an active Run.
    #[arg(long)]
    pub max_model_requests: Option<std::num::NonZeroU32>,
    /// Explicitly resume recovery; acceptance is not execution success. Track the returned Turn with wait/turn get.
    #[arg(long)]
    pub resume_recovery: bool,
    /// Self-asserted name (at most 64 Unicode characters); grants no authority.
    #[arg(long)]
    pub sender_label: Option<String>,
    /// Recipient Thread; omit to send to your current Thread. This does not ask a person or require a reply.
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    /// Omitted: the next safe boundary. complete: when the Thread's live Run
    /// would complete, or immediately if none is live.
    #[arg(long, value_enum)]
    pub deliver: Option<wire::Deliver>,
    #[command(flatten)]
    pub text: TextOptions,
}
#[derive(Debug, Clone, Args)]
pub struct MessageListOptions {
    /// List pending reply duties.
    #[arg(long, required = true)]
    pub awaiting: bool,
    /// Requests that this Thread must answer (excludes named questions).
    #[arg(long = "owed-by-thread", conflicts_with_all = ["asked_by_thread_id", "to"])]
    pub owed_by_thread_id: Option<String>,
    /// Requests this model Thread sent, including its human questions.
    #[arg(long = "asked-by-thread", conflicts_with = "to")]
    pub asked_by_thread_id: Option<String>,
    /// Questions addressed to this named recipient.
    #[arg(long)]
    pub to: Option<String>,
    #[arg(long, default_value_t = wire::DEFAULT_LIST_LIMIT)]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, Args)]
pub struct MessageAskOptions {
    /// Self-asserted name (at most 64 Unicode characters); grants no authority.
    #[arg(long)]
    pub sender_label: Option<String>,
    /// Named recipient; defaults to human when no Thread is given.
    #[arg(long, conflicts_with = "thread_id")]
    pub to: Option<String>,
    /// Thread whose reply you want (not your own). Omit to ask a person (--to human is the default).
    #[arg(long = "thread")]
    pub thread_id: Option<String>,
    #[arg(long, value_enum, requires = "thread_id")]
    pub deliver: Option<wire::Deliver>,
    #[command(flatten)]
    pub text: TextOptions,
}
impl MessageAskOptions {
    fn into_command(self, text: String) -> wire::Command {
        wire::Command::MessageAsk(wire::MessageAskArgs {
            human: false,
            sender_label: self.sender_label,
            thread_id: self.thread_id,
            to: self.to,
            deliver: self.deliver.unwrap_or_default(),
            text,
        })
    }
}

#[derive(Debug, Parser)]
#[command(name = "/ask")]
struct AskSlash {
    #[command(flatten)]
    options: MessageAskOptions,
}

pub(crate) fn ask_slash(text: &str) -> Result<wire::Command, String> {
    let words = shlex::split(text).ok_or_else(|| "Unclosed quote or escape".to_owned())?;
    let AskSlash { mut options } =
        AskSlash::try_parse_from(std::iter::once("/ask").chain(words.iter().map(String::as_str)))
            .map_err(|error| error.to_string())?;
    let text = options
        .text
        .text
        .take()
        .ok_or("Use shellboy message ask --text-stdin for piped text.")?;
    let command = options.into_command(inline_text(text).map_err(|error| error.message)?);
    command.validate().map_err(|error| error.message)?;
    Ok(command)
}

#[derive(Debug, Clone, Args)]
pub struct MessageReplyOptions {
    /// ID from a received message's `Reply expected` preamble. Withdraw your own requests with message cancel.
    #[arg(long = "message")]
    pub message_id: String,
    #[arg(long, hide = true)]
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
    after_help = "Wait for a saved target:\n  shellboy wait --conditions-json '[{\"type\":\"shell_ended\",\"execution_id\":\"WORK_EXECUTION_ID\"}]' --timeout-ms 30000\n  shellboy wait --conditions-json '[{\"type\":\"turn_completed\",\"turn_id\":\"CHILD_TURN_ID\"}]' --timeout-ms 30000\n  shellboy wait --conditions-json '[{\"type\":\"reply_received\",\"message_id\":\"MESSAGE_ID\"}]' --timeout-ms 30000\n\nThread conditions: {\"type\":\"thread_settled\",\"thread_ids\":[\"A\",\"B\"],\"states\":[\"completed\",\"recovery_wait\",\"failed\",\"stopped\"]}. Reply condition: {\"type\":\"reply_received\",\"message_id\":\"MESSAGE_ID\"}. Use result.cursor with --after and unchanged conditions only for thread_settled. For shell_output, after satisfaction run shell read --execution ID --cursor CURSOR, then use the new read cursor inside the next shell_output condition (not --after).\n\nConditions use any semantics. timeout leaves the target running; satisfied means inspect target state, not guaranteed success. recovery_wait means inspect Run/Turn and choose recovery explicitly. Guide: shellboy docs get --id shellboy/delegation"
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
    #[arg(long = "shell-state")]
    pub shell_state: Option<String>,
}
#[derive(Debug, Clone, Args)]
pub struct HistoryWindowOptions {
    #[arg(long = "thread")]
    pub thread_id: String,
    /// Position on the selected path (0 is the first node); default is the end.
    #[arg(long, conflicts_with = "node_id")]
    pub ordinal: Option<u64>,
    /// Anchor at this node on the selected path.
    #[arg(long = "node")]
    pub node_id: Option<String>,
    /// With --ordinal: fail with cursor_expired unless this node still sits there.
    #[arg(long = "expect-node", requires = "ordinal")]
    pub expect_node_id: Option<String>,
    /// Items before the anchor; before + after + 1 is at most 200.
    #[arg(long, default_value_t = 20)]
    pub before: u32,
    /// Items after the anchor.
    #[arg(long, default_value_t = 20)]
    pub after: u32,
    /// Maximum Unicode scalar characters in each excerpt; zero omits its content.
    #[arg(long, default_value_t=wire::DEFAULT_HISTORY_EXCERPT_CHARS, value_parser=clap::value_parser!(u32).range(0..=wire::MAX_HISTORY_EXCERPT_CHARS as i64))]
    pub excerpt_chars: u32,
}
#[derive(Debug, Clone, Args)]
pub struct HistoryReadOptions {
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
impl ValueEnum for wire::ThreadListOrder {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Updated, Self::Created]
    }
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(match self {
            Self::Updated => "updated",
            Self::Created => "created",
        }))
    }
}
impl ValueEnum for wire::ThreadListState {
    fn value_variants<'a>() -> &'a [Self] {
        Self::ALL
    }
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.as_str()))
    }
}
impl ValueEnum for wire::ArchivedFilter {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Exclude, Self::Include, Self::Only]
    }
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.as_str()))
    }
}
impl ValueEnum for wire::Deliver {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Complete]
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
    /// Start at the latest bounded output instead of the retained beginning.
    #[arg(long, conflicts_with = "cursor")]
    pub tail: bool,
    /// Target work ID from shell start, separate from this observation shell ID.
    #[arg(long = "execution")]
    pub execution_id: String,
    /// result.cursor from the previous read of the same execution and encoding.
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub encoding: Option<String>,
    /// Include stdout/stderr ranges in the returned text.
    #[arg(long)]
    pub segments: bool,
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
    /// Also take in these kinds from the selected Threads or all history
    /// (user, summary, assistant_response, tool_result, spill, event, or all).
    /// Repeat to add several; they add to `indexing.auto_kinds` for this command.
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
}
#[derive(Debug, Clone, Args)]
pub struct IndexTargetOptions {
    #[arg(long, value_enum)]
    pub scope: Option<Scope>,
    #[arg(long = "space")]
    pub space_id: Option<String>,
    #[arg(long = "thread")]
    pub thread_ids: Vec<String>,
    #[arg(long = "source")]
    pub source_ids: Vec<String>,
}
impl From<IndexTargetOptions> for wire::IndexArgs {
    fn from(value: IndexTargetOptions) -> Self {
        Self {
            dimensions: None,
            scope: value.scope,
            space_id: value.space_id,
            thread_ids: value.thread_ids,
            source_ids: value.source_ids,
            kinds: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, Args)]
pub struct IndexRebuildOptions {
    #[command(flatten)]
    pub selection: IndexOptions,
    #[arg(long)]
    pub dimensions: Option<usize>,
}
#[derive(Debug, Clone, Args)]
pub struct IndexShrinkOptions {
    /// Report the effect and sizes without changing anything.
    #[arg(long, conflicts_with = "cancel")]
    pub dry_run: bool,
    /// Stop a running conversion and discard its partial result.
    #[arg(long)]
    pub cancel: bool,
    /// Switch back to the retained float32 index this one was converted from.
    #[arg(long, conflicts_with = "cancel")]
    pub revert: bool,
}
#[derive(Debug, Clone, Args)]
pub struct IndexRemoveOptions {
    /// The inactive index space to delete.
    #[arg(long = "space")]
    pub space_id: String,
    /// Report what would be deleted and the bytes freed without changing anything.
    #[arg(long)]
    pub dry_run: bool,
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
            kinds: value.kinds,
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

#[derive(Clone)]
enum IdPathSegment {
    Key(String),
    Index(usize),
}
struct IdSlot {
    path: Vec<IdPathSegment>,
    kind: wire::IdKind,
    value: String,
}

fn id_kind_for_field(field: &str) -> Option<wire::IdKind> {
    match field {
        "thread_id" | "thread_ids" | "parent_thread_id" => Some(wire::IdKind::Thread),
        "message_id" | "message_ids" => Some(wire::IdKind::Message),
        "run_id" | "recovery_run_id" => Some(wire::IdKind::Run),
        "execution_id" => Some(wire::IdKind::ShellExecution),
        _ => None,
    }
}

fn collect_id_slots(
    value: &serde_json::Value,
    path: &mut Vec<IdPathSegment>,
    slots: &mut Vec<IdSlot>,
) {
    match value {
        serde_json::Value::Object(object) => {
            for (field, child) in object {
                if let Some(kind) = id_kind_for_field(field) {
                    // Empty values are command-level sentinels, not IDs.
                    match child {
                        serde_json::Value::String(id) => {
                            if id.is_empty() {
                                continue;
                            }
                            path.push(IdPathSegment::Key(field.clone()));
                            slots.push(IdSlot {
                                path: path.clone(),
                                kind,
                                value: id.clone(),
                            });
                            path.pop();
                        }
                        serde_json::Value::Array(ids) => {
                            for (index, id) in ids.iter().enumerate() {
                                if let serde_json::Value::String(id) = id {
                                    if id.is_empty() {
                                        continue;
                                    }
                                    path.push(IdPathSegment::Key(field.clone()));
                                    path.push(IdPathSegment::Index(index));
                                    slots.push(IdSlot {
                                        path: path.clone(),
                                        kind,
                                        value: id.clone(),
                                    });
                                    path.pop();
                                    path.pop();
                                }
                            }
                        }
                        _ => {}
                    }
                } else {
                    path.push(IdPathSegment::Key(field.clone()));
                    collect_id_slots(child, path, slots);
                    path.pop();
                }
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(IdPathSegment::Index(index));
                collect_id_slots(child, path, slots);
                path.pop();
            }
        }
        _ => {}
    }
}

fn id_at_path_mut<'a>(
    value: &'a mut serde_json::Value,
    path: &[IdPathSegment],
) -> Option<&'a mut serde_json::Value> {
    let mut current = value;
    for segment in path {
        current = match segment {
            IdPathSegment::Key(key) => current.as_object_mut()?.get_mut(key)?,
            IdPathSegment::Index(index) => current.as_array_mut()?.get_mut(*index)?,
        };
    }
    Some(current)
}

/// Resolve a prefix using the same capability preflight for every online CLI client.
pub async fn resolve_owner_prefix(
    data: &Path,
    origin: Option<&wire::Origin>,
    kind: wire::IdKind,
    prefix: String,
    include_pending_duties: bool,
) -> Result<String, ApiError> {
    let response = crate::ipc::call_requiring_capability(
        data,
        wire::Command::IdResolve(wire::IdResolveArgs {
            kind,
            prefix,
            include_pending_duties,
        }),
        None,
        origin.cloned(),
        "id.resolve",
    )
    .await?;
    if !response.ok {
        return Err(response.error.unwrap_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidState,
                "Owner rejected ID resolution without an error",
            )
        }));
    }
    response
        .result
        .and_then(|result| result["id"].as_str().map(str::to_owned))
        .ok_or_else(|| ApiError::new(ErrorCode::InvalidState, "Owner returned no resolved ID"))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ConversationIds {
    thread_id: Option<String>,
    parent_thread_id: Option<String>,
}

pub async fn resolve_conversation_ids(
    data: &Path,
    origin: Option<&wire::Origin>,
    thread_id: &mut Option<String>,
    parent_thread_id: &mut Option<String>,
) -> Result<(), ApiError> {
    let resolved = resolve_id_arguments(
        ConversationIds {
            thread_id: thread_id.clone(),
            parent_thread_id: parent_thread_id.clone(),
        },
        |kind, prefix| resolve_owner_prefix(data, origin, kind, prefix, false),
    )
    .await?;
    *thread_id = resolved.thread_id;
    *parent_thread_id = resolved.parent_thread_id;
    Ok(())
}

/// Resolve every target ID in typed CLI input before sending it over the wire.
pub async fn resolve_id_arguments<T, F, Fut>(input: T, mut resolve_prefix: F) -> Result<T, ApiError>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    F: FnMut(wire::IdKind, String) -> Fut,
    Fut: Future<Output = Result<String, ApiError>>,
{
    let mut value = serde_json::to_value(&input)
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Cannot inspect CLI target IDs"))?;
    let mut slots = Vec::new();
    collect_id_slots(&value, &mut Vec::new(), &mut slots);
    let mut prefixes = Vec::new();
    for slot in slots {
        match slot.value.chars().count() {
            36 => {}
            8..=35 => prefixes.push(slot),
            _ => {
                return Err(invalid("ID arguments must be a full UUID or an 8-character-or-longer prefix")
                    .hint("Pass the full ID or at least its first 8 characters")
                    .details(serde_json::json!({"reason":"id_prefix_too_short_or_long","minimum_prefix_chars":8,"full_id_chars":36})));
            }
        }
    }
    for slot in prefixes {
        let resolved = resolve_prefix(slot.kind, slot.value).await?;
        let Some(value) = id_at_path_mut(&mut value, &slot.path) else {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "CLI ID argument path changed during resolution",
            ));
        };
        *value = serde_json::Value::String(resolved);
    }
    serde_json::from_value(value)
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Resolved CLI command is invalid"))
}

impl Cli {
    pub async fn into_command(self, startup_cwd: &Path) -> Result<Option<wire::Command>, ApiError> {
        use std::io::IsTerminal;
        // A key typed at a terminal would be echoed; piping it is the only input path.
        if self.reads_key_from_stdin() && std::io::stdin().is_terminal() {
            return Err(invalid(
                "the key is read from standard input and is not accepted from a terminal, where it would be echoed; pipe it, e.g. printf %s \"$KEY\" | shellboy credentials set --connection NAME",
            ));
        }
        self.into_command_with_stdin(startup_cwd, &mut tokio::io::stdin())
            .await
    }
    fn reads_key_from_stdin(&self) -> bool {
        matches!(
            &self.command,
            Some(CliCommand::Credentials {
                command: CredentialsCommand::Set(_)
            }) | Some(CliCommand::Connection {
                command: ConnectionCommand::Test(ConnectionTestOptions {
                    api_key_stdin: true,
                    ..
                })
            })
        )
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
                since: a.since.map(String::into_boxed_str),
                since_node_id: a.since_node_id.map(String::into_boxed_str),
                until: a.until,
                thread_id: a.thread_id.map(String::into_boxed_str),
                run_id: a.run_id,
                turn_id: a.turn_id,
                include_children: a.include_children,
            }),
            CliCommand::Serve
            | CliCommand::Shutdown
            | CliCommand::Chat(_)
            | CliCommand::Exec(_)
            | CliCommand::Docs { .. }
            | CliCommand::Protocol { .. }
            | CliCommand::Startup(_)
            | CliCommand::Rag(_)
            | CliCommand::Config {
                command: ConfigCliCommand::Setup(_),
            } => {
                return Ok(None);
            }
            CliCommand::Credentials { .. } | CliCommand::Connection { .. }
                if self.config.is_some() =>
            {
                return Err(invalid(
                    "credentials and connection test act on the file the running owner loaded; --config cannot select another file. Choose the owner with --data-dir",
                ));
            }
            CliCommand::Credentials {
                command: CredentialsCommand::Set(a),
            } => C::CredentialsSet(wire::CredentialsSetArgs {
                connection: a.connection,
                value: read_key(stdin).await?,
            }),
            CliCommand::Credentials {
                command: CredentialsCommand::Clear(a),
            } => C::CredentialsClear(wire::CredentialsClearArgs {
                connection: a.connection,
            }),
            CliCommand::Connection {
                command: ConnectionCommand::Test(a),
            } => {
                let api = match a.api.as_deref() {
                    None => None,
                    Some(text) => Some(
                        serde_json::from_value(serde_json::Value::String(text.to_owned()))
                            .map_err(|_| {
                                invalid(
                                    "--api takes responses, chat_completions or anthropic_messages",
                                )
                            })?,
                    ),
                };
                if a.connection.is_none() && a.base_url.is_none() {
                    return Err(invalid("give --connection NAME or --base-url URL to test"));
                }
                C::ConnectionTest(wire::ConnectionTestArgs {
                    connection: a.connection,
                    base_url: a.base_url,
                    api,
                    api_key: if a.api_key_stdin {
                        Some(read_key(stdin).await?)
                    } else {
                        None
                    },
                    api_key_env: a.api_key_env,
                    api_key_header: a.api_key_header,
                    ssl_verify: a.no_ssl_verify.then_some(false),
                    post_model: a.post_model,
                })
            }
            CliCommand::Config { .. } if self.config.is_some() => {
                return Err(invalid(
                    "config get and config set act on the file the running owner loaded; --config cannot select another file. Choose the owner with --data-dir",
                ));
            }
            CliCommand::Config {
                command: ConfigCliCommand::Get,
            } => C::ConfigGet(wire::ConfigGetArgs {}),
            CliCommand::Config {
                command: ConfigCliCommand::Set(a),
            } => C::ConfigSet(a.into_args()?),
            CliCommand::Model {
                command: ModelCommand::List(a),
            } => C::ModelList(wire::ModelListArgs {
                connection: a.connection,
                thread_id: a.thread_id,
            }),
            CliCommand::Thread { command } => match command {
                ThreadCommand::Request(_) | ThreadCommand::Export(_) => return Ok(None),
                ThreadCommand::Import { input } => C::ThreadImport(wire::ThreadImportArgs {
                    document: crate::thread_import::read_file(&input)?,
                }),
                ThreadCommand::List(a) => C::ThreadList(wire::ThreadListArgs {
                    thread_ids: None,
                    state: a.state,
                    order: a.order,
                    limit: a.limit,
                    cursor: a.cursor,
                    query: a.query,
                    awaiting: a.awaiting,
                    archived: a.archived,
                    tags: a.tags,
                    cwd: a
                        .cwd
                        .map(|path| {
                            if path.as_os_str().is_empty() {
                                Err(invalid("--cwd must not be empty"))
                            } else {
                                absolute_attachment(path, startup_cwd)
                            }
                        })
                        .transpose()?,
                }),
                ThreadCommand::Tag(a) => C::ThreadTag(wire::ThreadTagArgs {
                    thread_ids: a.thread_ids,
                    add: a.add,
                    remove: a.remove,
                }),
                ThreadCommand::Tags(a) => C::ThreadTags(wire::ThreadTagsArgs {
                    limit: a.limit,
                    cursor: a.cursor,
                }),
                ThreadCommand::Cwds(a) => C::ThreadCwds(wire::ThreadCwdsArgs {
                    limit: a.limit,
                    cursor: a.cursor,
                }),
                ThreadCommand::Archive(a) => C::ThreadArchive(wire::ThreadArchiveArgs {
                    thread_ids: a.thread_ids,
                    descendants: a.descendants,
                }),
                ThreadCommand::Unarchive(a) => C::ThreadUnarchive(wire::ThreadArchiveArgs {
                    thread_ids: a.thread_ids,
                    descendants: a.descendants,
                }),
                ThreadCommand::Get(a) => C::ThreadGet(crate::protocol::ThreadGetArgs {
                    thread_id: a.thread_id,
                    display: false,
                }),
                ThreadCommand::Create(a) => C::ThreadCreate(wire::ThreadCreateArgs {
                    agents: path_option(a.agents)?,
                    connection: a.connection,
                    cwd: path_option(a.cwd)?,
                    model: a.model,
                    effort: a.effort,
                    parent_thread_id: a.parent_thread_id,
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
                ThreadCommand::Branches(a) => C::ThreadBranches(wire::ThreadBranchesArgs {
                    thread_id: a.thread_id,
                    limit: a.limit,
                    cursor: a.cursor,
                }),
                ThreadCommand::Rewind(a) => C::ThreadRewind(wire::ThreadRewindArgs {
                    thread_id: a.thread_id,
                    node_id: a.node_id,
                    exclude_node: a.exclude_node,
                    dry_run: a.dry_run,
                    recovery_run_id: a.run_id,
                }),
                ThreadCommand::Set(a) => C::ThreadSet(Box::new(wire::ThreadSetArgs {
                    agents: path_option(a.agents)?,
                    title: a.title,
                    connection: a.connection,
                    thread_id: a.thread_id,
                    cwd: path_option(a.cwd)?,
                    model: a.model,
                    effort: a.effort,
                    auto_compact_at: a.auto_compact_at,
                    auto_compact_target_percent: a.auto_compact_target_percent,
                    summary_max_chars: a.summary_max_chars,
                    max_tokens: a.max_tokens,
                    spill: a.spill,
                    parent_thread_id: a.parent_thread_id,
                })),
            },
            CliCommand::Message { command } => match command {
                MessageCommand::Send(a) => C::MessageSend(wire::MessageSendArgs {
                    max_model_requests: a.max_model_requests,
                    human: false,
                    resume_recovery: a.resume_recovery,
                    sender_label: a.sender_label,
                    thread_id: a.thread_id,
                    text: read_text(a.text, stdin).await?,
                    deliver: a.deliver.unwrap_or_default(),
                }),
                MessageCommand::List(a) => C::MessageList(wire::MessageListArgs {
                    owed_by_thread_id: a.owed_by_thread_id,
                    asked_by_thread_id: a.asked_by_thread_id,
                    to: a.to,
                    limit: a.limit,
                    cursor: a.cursor,
                }),
                MessageCommand::Ask(mut a) => {
                    let text = read_text(std::mem::take(&mut a.text), stdin).await?;
                    a.into_command(text)
                }
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
            CliCommand::Turn {
                command: TurnCommand::Interrupt(a),
            } => C::TurnInterrupt(wire::TurnTarget { turn_id: a.turn_id }),
            CliCommand::Wait(a) => C::Wait(wire::WaitArgs {
                conditions: serde_json::from_str(&a.conditions_json).map_err(|_| {
                    invalid("conditions-json must be an array of typed wait conditions")
                })?,
                timeout_ms: a.timeout_ms,
                after: a.after,
            }),
            CliCommand::Events {
                command: EventsCommand::Wait(a),
            } => C::EventsWait(wire::EventsWaitArgs {
                after: a.after,
                thread_ids: a.thread_ids,
                timeout_ms: a.timeout_ms,
                include_text: a.text,
                include_reasoning_activity: a.reasoning_activity,
            }),
            CliCommand::History { command } => match command {
                HistoryCommand::List(a) => C::HistoryList(wire::HistoryListArgs {
                    excerpt_chars: a.excerpt_chars,
                    thread_id: a.thread_id,
                    limit: a.limit,
                    cursor: a.cursor,
                    run_id: a.run_id,
                    since: a.since,
                    shell_state: a.shell_state,
                }),
                HistoryCommand::Window(a) => C::HistoryWindow(wire::HistoryWindowArgs {
                    items: false,
                    thread_id: a.thread_id,
                    ordinal: a.ordinal,
                    node_id: a.node_id,
                    expect_node_id: a.expect_node_id,
                    before: a.before,
                    after: a.after,
                    excerpt_chars: a.excerpt_chars,
                }),
                HistoryCommand::Read(a) => C::HistoryRead(wire::HistoryReadArgs {
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
            CliCommand::Artifact { command } => match command {
                ArtifactCommand::Add {
                    thread_id,
                    uri,
                    title,
                    mime,
                } => C::ArtifactAdd(wire::ArtifactAddArgs {
                    thread_id,
                    uri: artifact_uri_from_cli(uri, startup_cwd)?,
                    title,
                    mime,
                }),
                ArtifactCommand::Update {
                    artifact_id,
                    uri,
                    title,
                    mime,
                } => C::ArtifactUpdate(wire::ArtifactUpdateArgs {
                    artifact_id,
                    uri: artifact_uri_from_cli(uri, startup_cwd)?,
                    title,
                    mime,
                }),
                ArtifactCommand::List {
                    thread_id,
                    cursor,
                    limit,
                } => C::ArtifactList(wire::ArtifactListArgs {
                    thread_id,
                    cursor,
                    limit,
                }),
                ArtifactCommand::Get {
                    artifact_id,
                    version,
                    cursor,
                    limit,
                } => C::ArtifactGet(wire::ArtifactGetArgs {
                    artifact_id,
                    version,
                    cursor,
                    limit,
                }),
                ArtifactCommand::Remove { artifact_id } => {
                    C::ArtifactRemove(wire::ArtifactRemoveArgs { artifact_id })
                }
            },
            CliCommand::Service { command } => command.into(),
            CliCommand::Schedule { command } => command.into(),
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
                    tail: a.tail,
                    segments: a.segments,
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
                ShellCommand::Background(a) => C::ShellBackground(wire::ExecutionTarget {
                    execution_id: a.execution_id,
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
                AttachmentCommand::List(a) => C::AttachmentList(wire::AttachmentListArgs {
                    thread_id: a.thread_id,
                    limit: a.limit,
                    cursor: a.cursor,
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

                if (a.max_chars.is_some()) && !a.summary {
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
                IndexCommand::Shrink(a) => C::IndexShrink(wire::IndexShrinkArgs {
                    dry_run: a.dry_run,
                    cancel: a.cancel,
                    revert: a.revert,
                }),
                IndexCommand::Remove(a) => C::IndexRemove(wire::IndexRemoveArgs {
                    space_id: a.space_id,
                    dry_run: a.dry_run,
                }),
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
fn inline_text(text: String) -> Result<String, ApiError> {
    if text.len() > MAX_STDIN_TEXT_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "text exceeds the CLI byte limit",
        ));
    }
    Ok(text)
}
async fn read_text<R: AsyncRead + Unpin>(
    options: TextOptions,
    reader: &mut R,
) -> Result<String, ApiError> {
    let max_bytes = MAX_STDIN_TEXT_BYTES;
    match (options.text, options.text_stdin) {
        (Some(text), false) => inline_text(text),
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

fn artifact_uri_from_cli(uri: String, cwd: &Path) -> Result<String, ApiError> {
    let path = Path::new(&uri);
    let has_scheme = uri.split_once(':').is_some_and(|(name, _)| {
        let mut bytes = name.bytes();
        bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    });
    // Preserve Windows drive paths and UNC paths even on a Unix CLI, and vice versa.
    if uri.is_empty()
        || path.is_absolute()
        || uri.starts_with('/')
        || uri.starts_with(r"\\")
        || has_scheme
    {
        return Ok(uri);
    }
    cwd.join(path).into_os_string().into_string().map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidArgument,
            "The absolute artifact path is not valid UTF-8",
        )
    })
}
