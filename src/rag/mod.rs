//! Independent document retrieval. Never starts a conversation owner.
mod builtin;
pub mod extract;
mod history;
mod provider_breaker;
pub(crate) use crate::digest::sha256_hex as hash;
use provider_breaker::ProviderBreaker;
mod search;
mod store;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(crate) const MAX_INPUT: usize = 16 * 1024 * 1024;
pub(crate) const SPLIT: usize = 2000;
#[derive(Debug, Clone, Args)]
pub struct RagOptions {
    /// Independent knowledge database; defaults to DATA_DIR/rag.db.
    #[arg(long, global = true)]
    pub db: Option<PathBuf>,
    #[command(subcommand)]
    pub command: RagCommand,
}
#[derive(Debug, Clone, Subcommand)]
pub enum RagCommand {
    Init,
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    Index {
        /// Registered source ID/name, or docs for bundled guides; omitted targets registered materials.
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, value_enum, default_value = "keyword")]
        mode: IndexMode,
        #[arg(long)]
        extractor: Option<PathBuf>,
        #[arg(long)]
        adapter: Option<PathBuf>,
    },
    Ingest {
        #[arg(long)]
        id: String,
        #[arg(long)]
        source_file: PathBuf,
        #[arg(long, value_enum)]
        kind: IngestKind,
        #[arg(
            long,
            conflicts_with = "text_stdin",
            required_unless_present = "text_stdin"
        )]
        text_file: Option<PathBuf>,
        #[arg(long, conflicts_with = "text_file")]
        text_stdin: bool,
        #[arg(long)]
        metadata_file: Option<PathBuf>,
    },
    Status,
    Inspect {
        #[arg(long)]
        id: String,
    },
    Search {
        #[arg(long)]
        query: String,
        #[arg(long)]
        scope: Vec<String>,
        #[arg(long, value_enum, default_value = "keyword")]
        mode: SearchMode,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    Get {
        #[arg(long = "ref")]
        reference: String,
        #[arg(long, default_value_t = 16384)]
        limit_bytes: u32,
        #[arg(long)]
        cursor: Option<String>,
    },
}
#[derive(Debug, Clone, Subcommand)]
pub enum SourceCommand {
    Add {
        path: PathBuf,
        #[arg(long)]
        name: String,
        /// Persistent source-relative globs; repeat to exclude multiple patterns.
        #[arg(long, conflicts_with = "clear_excludes")]
        exclude: Vec<String>,
        /// Explicitly remove existing exclusion patterns when re-registering a source.
        #[arg(long)]
        clear_excludes: bool,
    },
    List,
    Remove {
        #[arg(long)]
        source: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IndexMode {
    Keyword,
    Semantic,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchMode {
    Keyword,
    Semantic,
    Hybrid,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IngestKind {
    Extracted,
    LlmExplanation,
    Summary,
}

#[derive(Debug)]
pub(crate) struct Error {
    pub code: String,
    pub message: String,
}
pub(crate) type Result<T> = std::result::Result<T, Error>;
pub(crate) fn err(code: &str, message: impl Into<String>) -> Error {
    Error {
        code: code.into(),
        message: message.into(),
    }
}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        err(
            "database_failed",
            "Knowledge database operation failed; existing data was preserved",
        )
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        err(
            "io_failed",
            "Cannot read or write the requested local resource",
        )
    }
}
pub(crate) fn encode<T: Serialize>(value: &T) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("serializable reference"))
}
pub(crate) fn decode<T: for<'de> Deserialize<'de>>(token: &str) -> Result<T> {
    if token.len() > 8192 {
        return Err(err(
            "invalid_argument",
            "Reference or cursor exceeds its limit",
        ));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| err("invalid_argument", "Malformed reference or cursor"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| err("invalid_argument", "Malformed reference or cursor"))
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reference {
    pub version: u32,
    pub store: String,
    pub kind: String,
    pub id: String,
    pub revision: String,
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<Value>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u32,
    reference: String,
    offset: usize,
}
pub(crate) fn page(text: &str, reference: &str, limit: u32, cursor: Option<&str>) -> Result<Value> {
    if !(1..=65536).contains(&limit) {
        return Err(err("invalid_argument", "limit-bytes must be 1..=65536"));
    }
    let start = if let Some(token) = cursor {
        let position: Cursor =
            decode(token).map_err(|_| err("cursor_expired", "Invalid retrieval cursor"))?;
        if position.version != 1 || position.reference != reference {
            return Err(err("cursor_expired", "Cursor belongs to another reference"));
        }
        position.offset
    } else {
        0
    };
    if start > text.len() || !text.is_char_boundary(start) {
        return Err(err("cursor_expired", "Retrieval position is unavailable"));
    }
    let end = text.floor_char_boundary(text.len().min(start.saturating_add(limit as usize)));
    if end == start && end < text.len() {
        return Err(err(
            "invalid_argument",
            "limit-bytes must fit the next UTF-8 character",
        ));
    }
    let next = (end < text.len()).then(|| {
        encode(&Cursor {
            version: 1,
            reference: reference.into(),
            offset: end,
        })
    });
    Ok(
        json!({"text": &text[start..end], "next_cursor":next,"truncated":next.is_some(),"offset_bytes":start,"total_bytes":text.len()}),
    )
}
pub(crate) fn bounded_read(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    if !std::fs::metadata(path)?.is_file() {
        return Err(err("invalid_argument", "Input must be a regular file"));
    }
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_INPUT as u64 {
        return Err(err("input_limit", "Input exceeds 16 MiB"));
    }
    let mut result = Vec::new();
    file.take(MAX_INPUT as u64 + 1).read_to_end(&mut result)?;
    if result.len() > MAX_INPUT {
        return Err(err("input_limit", "Input exceeds 16 MiB"));
    }
    Ok(result)
}
pub(crate) fn absolute(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        cwd.join(path)
    }
}

/// Blocking storage and extraction stay on one finite standalone worker; network
/// futures use the shared runtime. No UI/owner thread or conversation writer starts.
pub async fn execute(
    options: RagOptions,
    config: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    cwd: &Path,
) -> Value {
    let cwd = cwd.to_path_buf();
    let handle = tokio::runtime::Handle::current();
    match tokio::task::spawn_blocking(move || handle.block_on(run(options, config, data_dir, &cwd)))
        .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":false,"error":{"code":error.code,"message":error.message}})
        }
        Err(_) => {
            json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":false,"error":{"code":"worker_failed","message":"Standalone retrieval worker failed"}})
        }
    }
}
async fn run(
    options: RagOptions,
    config: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    cwd: &Path,
) -> Result<Value> {
    let directory = crate::config::resolve_cli_data_dir(
        data_dir.as_deref(),
        &crate::config::PathEnvironment::current(),
        cwd,
    )
    .map_err(|_| err("config_invalid", "Cannot resolve data directory"))?;
    let database = options
        .db
        .as_ref()
        .map(|p| absolute(p, cwd))
        .unwrap_or_else(|| directory.join("rag.db"));
    let context = search::Context {
        database,
        history: directory.join("shellboy.db"),
        config,
        data_dir,
        cwd: cwd.into(),
    };
    let value = match options.command {
        RagCommand::Init => store::initialize(&context.database)?,
        RagCommand::Source { command } => store::source_command(&context.database, command, cwd)?,
        RagCommand::Index {
            source,
            dry_run,
            mode,
            extractor,
            adapter,
        } => {
            store::index(
                &context,
                source.as_deref(),
                dry_run,
                mode,
                extractor.as_deref(),
                adapter.as_deref(),
            )
            .await?
        }
        RagCommand::Ingest {
            id,
            source_file,
            kind,
            text_file,
            text_stdin,
            metadata_file,
        } => store::ingest(
            &context.database,
            &id,
            &absolute(&source_file, cwd),
            kind,
            text_file.as_deref().map(|p| absolute(p, cwd)).as_deref(),
            text_stdin,
            metadata_file
                .as_deref()
                .map(|p| absolute(p, cwd))
                .as_deref(),
        )?,
        RagCommand::Status => store::status(&context)?,
        RagCommand::Inspect { id } => store::inspect(&context.database, &id)?,
        RagCommand::Search {
            query,
            scope,
            mode,
            limit,
        } => return search::search(&context, &query, &scope, mode, limit).await,
        RagCommand::Get {
            reference,
            limit_bytes,
            cursor,
        } => search::get(&context, &reference, limit_bytes, cursor.as_deref())?,
    };
    if value.get("partial_failure").and_then(Value::as_bool) == Some(true) {
        Ok(
            json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":false,"result":value,"error":{"code":"partial_failure","message":"Some inputs failed; successful revisions were preserved"}}),
        )
    } else {
        Ok(json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":true,"result":value}))
    }
}
