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
        /// Repeat docs, history, history:THREAD, or source:ID/name.
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
    // This CLI wrapper alone may consume the process terminal/pipe. UI callers
    // always use execute_with_text and can never fall back to global stdin.
    let text = match cli_text(&options, &mut tokio::io::stdin()).await {
        Ok(text) => text,
        Err(error) => return failure(error),
    };
    execute_with_text(options, config, data_dir, cwd, text).await
}
fn failure(error: Error) -> Value {
    json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":false,"error":{"code":error.code,"message":error.message}})
}
async fn cli_text<R: tokio::io::AsyncRead + Unpin>(
    options: &RagOptions,
    input: &mut R,
) -> Result<Option<String>> {
    use tokio::io::AsyncReadExt;
    let RagCommand::Ingest {
        text_file,
        text_stdin,
        ..
    } = &options.command
    else {
        return Ok(None);
    };
    if !*text_stdin {
        return Ok(None);
    }
    if text_file.is_some() {
        return Err(err(
            "invalid_argument",
            "Choose text-file or text-stdin, not both",
        ));
    }
    let mut bytes = Vec::new();
    input
        .take(MAX_INPUT as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > MAX_INPUT {
        return Err(err("input_limit", "Input exceeds 16 MiB"));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| err("invalid_argument", "Derived text must be UTF-8"))
}
fn validate_owned_text(options: &RagOptions, text: Option<&str>) -> Result<()> {
    match &options.command {
        RagCommand::Ingest {
            text_file,
            text_stdin,
            ..
        } => {
            if text.is_some() && text_file.is_some() {
                return Err(err(
                    "invalid_argument",
                    "Choose owned text or text-file, not both",
                ));
            }
            if text.is_none() && (*text_stdin || text_file.is_none()) {
                return Err(err(
                    "invalid_argument",
                    "Owned text is required; this entrypoint never reads stdin",
                ));
            }
            if text.is_some_and(|text| text.len() > MAX_INPUT) {
                return Err(err("input_limit", "Input exceeds 16 MiB"));
            }
        }
        _ if text.is_some() => {
            return Err(err(
                "invalid_argument",
                "Owned input is only supported for ingest",
            ));
        }
        _ => {}
    }
    Ok(())
}
/// Shared standalone/UI entrypoint. All input is owned or an explicit file;
/// there is no terminal reader anywhere below this boundary.
pub(crate) async fn execute_with_text(
    options: RagOptions,
    config: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    cwd: &Path,
    text: Option<String>,
) -> Value {
    if let Err(error) = validate_owned_text(&options, text.as_deref()) {
        return failure(error);
    }
    let cwd = cwd.to_path_buf();
    let handle = tokio::runtime::Handle::current();
    match tokio::task::spawn_blocking(move || {
        handle.block_on(run(options, config, data_dir, &cwd, text))
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => failure(error),
        Err(_) => failure(err("worker_failed", "Standalone retrieval worker failed")),
    }
}

async fn run(
    options: RagOptions,
    config: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    cwd: &Path,
    owned_text: Option<String>,
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
        } => store::index(&context, source.as_deref(), dry_run, mode).await?,
        RagCommand::Ingest {
            id,
            source_file,
            kind,
            text_file,
            text_stdin: _,
            metadata_file,
        } => store::ingest(
            &context.database,
            &id,
            &absolute(&source_file, cwd),
            kind,
            text_file.as_deref().map(|p| absolute(p, cwd)).as_deref(),
            owned_text,
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

#[cfg(test)]
mod owned_ingest_tests {
    use super::*;
    fn options(
        db: PathBuf,
        source: PathBuf,
        text_file: Option<PathBuf>,
        stdin: bool,
    ) -> RagOptions {
        RagOptions {
            db: Some(db),
            command: RagCommand::Ingest {
                id: "derived".into(),
                source_file: source,
                kind: IngestKind::Summary,
                text_file,
                text_stdin: stdin,
                metadata_file: None,
            },
        }
    }
    #[tokio::test]
    async fn owned_ingest_and_cli_file_share_provenance_and_body_hash() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("source.bin");
        let text = root.path().join("derived.txt");
        std::fs::write(&original, b"original bytes").unwrap();
        std::fs::write(&text, "Derived 日本語 text").unwrap();
        let first = root.path().join("file.db");
        let second = root.path().join("owned.db");
        let data = Some(root.path().join("data"));
        for path in [&first, &second] {
            let value = execute_with_text(
                RagOptions {
                    db: Some(path.clone()),
                    command: RagCommand::Init,
                },
                None,
                data.clone(),
                root.path(),
                None,
            )
            .await;
            assert_eq!(value["ok"], true, "{value}");
        }
        let file = execute(
            options(first.clone(), original.clone(), Some(text), false),
            None,
            data.clone(),
            root.path(),
        )
        .await;
        let owned = execute_with_text(
            options(second.clone(), original, None, true),
            None,
            data.clone(),
            root.path(),
            Some("Derived 日本語 text".into()),
        )
        .await;
        assert_eq!(file["ok"], true, "{file}");
        assert_eq!(owned["ok"], true, "{owned}");
        let mut provenances = Vec::new();
        for (path, ingested) in [(first, &file), (second, &owned)] {
            let value = execute_with_text(
                RagOptions {
                    db: Some(path),
                    command: RagCommand::Inspect {
                        id: ingested["result"]["document_id"].as_str().unwrap().into(),
                    },
                },
                None,
                data.clone(),
                root.path(),
                None,
            )
            .await;
            assert_eq!(value["ok"], true, "{value}");
            provenances.push(value["result"]["provenance"].clone());
        }
        assert_eq!(provenances[0], provenances[1]);
        assert_eq!(
            provenances[0]["derived_body_hash"],
            hash("Derived 日本語 text".as_bytes())
        );
    }
    #[tokio::test]
    async fn owned_entrypoint_rejects_missing_conflicting_and_oversize_input_before_creating_db() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("must-not-exist.db");
        let source = root.path().join("missing-source");
        let data = Some(root.path().join("data"));
        let missing = execute_with_text(
            options(db.clone(), source.clone(), None, true),
            None,
            data.clone(),
            root.path(),
            None,
        )
        .await;
        assert_eq!(missing["ok"], false);
        assert_eq!(missing["error"]["code"], "invalid_argument");
        assert!(
            missing["error"]["message"]
                .as_str()
                .unwrap()
                .contains("never reads stdin")
        );
        let conflict = execute_with_text(
            options(db.clone(), source.clone(), Some("text.txt".into()), true),
            None,
            data.clone(),
            root.path(),
            Some("owned".into()),
        )
        .await;
        assert_eq!(conflict["error"]["code"], "invalid_argument");
        let too_large = execute_with_text(
            options(db.clone(), source, None, false),
            None,
            data.clone(),
            root.path(),
            Some("x".repeat(MAX_INPUT + 1)),
        )
        .await;
        assert_eq!(too_large["error"]["code"], "input_limit");
        let non_ingest = execute_with_text(
            RagOptions {
                db: Some(db.clone()),
                command: RagCommand::Init,
            },
            None,
            data,
            root.path(),
            Some("unused".into()),
        )
        .await;
        assert_eq!(non_ingest["error"]["code"], "invalid_argument");
        assert!(!db.exists());
    }
    #[tokio::test]
    async fn cli_injectable_input_keeps_utf8_limit_and_explicit_source_validation() {
        use tokio::io::AsyncReadExt;
        let command = options("unused.db".into(), "source".into(), None, true);
        let mut valid = "日本語".as_bytes();
        assert_eq!(
            cli_text(&command, &mut valid).await.unwrap(),
            Some("日本語".into())
        );
        let mut invalid = &b"\xff"[..];
        assert_eq!(
            cli_text(&command, &mut invalid).await.err().unwrap().code,
            "invalid_argument"
        );
        let mut large = tokio::io::repeat(b'x').take(MAX_INPUT as u64 + 1);
        assert_eq!(
            cli_text(&command, &mut large).await.err().unwrap().code,
            "input_limit"
        );
        let conflict = options(
            "unused.db".into(),
            "source".into(),
            Some("text.txt".into()),
            true,
        );
        let mut empty = tokio::io::empty();
        assert_eq!(
            cli_text(&conflict, &mut empty).await.err().unwrap().code,
            "invalid_argument"
        );
    }
}
