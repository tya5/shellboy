//! `shellboy thread request` (#137): assemble a conversation request from saved
//! state without sending it. Standalone and read-only: it opens the conversation
//! database read-only, never starts an owner, never calls a provider, and never
//! prints headers or credentials. Parts that saved state cannot determine are
//! reported as unknown rather than guessed.
use crate::{
    config::{Config, LoadedConfig, PathEnvironment},
    control::replay::{PathWitness, load_uncached},
    control::request_parts::{
        ContextLine, context_item, has_attachment_ref, leading_items, strip_local,
    },
    model::http::HttpModelClient,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Characters of an item hash shown in the item list.
const SHORT_HASH: usize = 12;

#[derive(Debug, Clone, clap::Args)]
pub struct RequestOptions {
    /// Thread whose request to assemble.
    #[arg(long = "thread")]
    pub thread_id: String,
    /// Rebuild the request a saved model attempt sent, instead of the next one.
    #[arg(long = "attempt")]
    pub attempt_id: Option<String>,
    /// Also report the first item that differs from this saved attempt's request.
    #[arg(long = "compare-attempt")]
    pub compare_attempt_id: Option<String>,
    /// Include the request body JSON; configured extra_body values are shown as hashes.
    #[arg(long)]
    pub body: bool,
}

/// A coded inspection failure; `message` never contains request content.
#[derive(Debug)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}
pub type Result<T> = std::result::Result<T, Failure>;
fn fail(code: &'static str, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}
impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        fail("storage_failed", error.to_string())
    }
}
impl From<crate::storage::StorageError> for Failure {
    fn from(error: crate::storage::StorageError) -> Self {
        fail("storage_failed", error.to_string())
    }
}
impl From<crate::protocol::ApiError> for Failure {
    fn from(error: crate::protocol::ApiError) -> Self {
        fail("invalid_state", error.message.to_string())
    }
}

/// CLI entry point: one JSON response value.
pub async fn execute(
    options: RequestOptions,
    config: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    cwd: &Path,
) -> Value {
    let cwd = cwd.to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        let loaded = Config::load_with_overrides(
            config.as_deref(),
            data_dir.as_deref(),
            &cwd,
            &PathEnvironment::current(),
        )
        .map_err(|error| fail("config_invalid", error.to_string()))?;
        inspect(&loaded, &options)
    })
    .await
    .unwrap_or_else(|_| Err(fail("io_failed", "Request inspection worker failed")));
    match result {
        Ok(result) => {
            json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":true,"result":result})
        }
        Err(error) => {
            json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":false,"error":{"code":error.code,"message":error.message}})
        }
    }
}

/// Assemble and describe the request. Blocking; reads the database read-only.
pub fn inspect(loaded: &LoadedConfig, options: &RequestOptions) -> Result<Value> {
    let path = loaded.data_dir.join("shellboy.db");
    if !path.is_file() {
        return Err(fail(
            "not_found",
            "No conversation database in the data directory",
        ));
    }
    let db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    // One read transaction: every row comes from the same database snapshot.
    let tx = db.unchecked_transaction()?;
    let thread = &options.thread_id;
    let primary = match &options.attempt_id {
        Some(attempt) => attempt_plan(&tx, thread, attempt)?,
        None => next_plan(&tx, thread)?,
    };
    let primary = assemble(&loaded.config, &tx, primary)?;
    let mut result = primary.describe(options.body, &loaded.config);
    if let Some(other) = &options.compare_attempt_id {
        let other = assemble(&loaded.config, &tx, attempt_plan(&tx, thread, other)?)?;
        result["compare"] = compare(&primary, &other);
    }
    Ok(result)
}

/// What saved state says about one request.
struct Plan {
    source: &'static str,
    attempt: Option<String>,
    thread: String,
    run: String,
    turn: String,
    profile: String,
    model: String,
    effort: Option<String>,
    epoch: i64,
    cwd: Option<String>,
    agents: PathBuf,
    /// Outer None: unknown. Inner None: no instructions were present.
    instructions: Option<Option<String>>,
    /// Some(nodes) when the replayed path is known; None reads the whole path.
    history: History,
    attachments_known_empty: bool,
    saved: Option<Value>,
}
enum History {
    Current,
    Prefix(PathWitness),
    Unknown(&'static str),
}

/// run, instruction source, body, observed size, Thread AGENTS path.
type TurnRow = (String, Option<String>, Option<String>, Option<i64>, String);
/// turn, connection, model, effort, reasoning epoch, manifest, usage.
type AttemptRow = (
    String,
    String,
    String,
    Option<String>,
    i64,
    String,
    Option<String>,
);

fn turn_row(
    tx: &Connection,
    thread: &str,
    turn: &str,
) -> Result<(String, PathBuf, Option<Option<String>>)> {
    let row: Option<TurnRow> = tx
        .query_row(
            "SELECT u.run_id,s.source_path,s.body,s.observed_size,t.agents_path FROM turn u JOIN thread t ON t.id=u.thread_id LEFT JOIN instruction_snapshot s ON s.id=u.instruction_snapshot_id WHERE u.id=?1 AND u.thread_id=?2",
            params![turn, thread],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((run, source, body, size, agents)) = row else {
        return Err(fail("not_found", "Turn is not in this Thread"));
    };
    let instructions = source.is_some().then(|| size.and(body));
    Ok((run, PathBuf::from(source.unwrap_or(agents)), instructions))
}

fn attempt_plan(tx: &Connection, thread: &str, attempt: &str) -> Result<Plan> {
    let row: Option<AttemptRow> = tx
        .query_row(
            "SELECT a.turn_id,a.profile_id,a.model_id,a.effort,a.reasoning_epoch,a.input_manifest_json,a.usage_json FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE a.id=?1 AND u.thread_id=?2",
            params![attempt, thread],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((turn, profile, model, effort, epoch, manifest, usage)) = row else {
        return Err(fail("not_found", "Model attempt is not in this Thread"));
    };
    let manifest: Value = serde_json::from_str(&manifest)
        .map_err(|_| fail("invalid_state", "Saved input manifest is not JSON"))?;
    let (run, agents, instructions) = turn_row(tx, thread, &turn)?;
    let history = match serde_json::from_value::<PathWitness>(manifest["replay"].clone()) {
        Ok(witness) => History::Prefix(witness),
        Err(_) => History::Unknown("this attempt's manifest does not record its replayed path"),
    };
    let usage = usage
        .and_then(|usage| serde_json::from_str::<Value>(&usage).ok())
        .map(|usage| {
            json!({
                "input_tokens": usage["input_tokens"],
                "cached_tokens": usage["input_tokens_details"]["cached_tokens"],
            })
        });
    Ok(Plan {
        source: "attempt",
        attempt: Some(attempt.to_owned()),
        thread: thread.to_owned(),
        run,
        turn,
        profile,
        model,
        effort,
        epoch,
        cwd: manifest["cwd"].as_str().map(str::to_owned),
        agents,
        instructions,
        history,
        attachments_known_empty: manifest["attachments"]
            .as_array()
            .is_some_and(Vec::is_empty),
        saved: Some(json!({
            "input_characters": manifest["input_characters"],
            "request_body_bytes": manifest["http_observation"]["request_body_bytes"],
            "usage": usage,
        })),
    })
}

/// The next request of the Thread's latest Turn from the current saved path.
/// Pending deliveries not yet appended to the path are not included.
fn next_plan(tx: &Connection, thread: &str) -> Result<Plan> {
    let row: Option<(String, String, Option<String>, i64, String)> = tx
        .query_row(
            "SELECT profile_id,model_id,effort,reasoning_epoch,cwd FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [thread],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((profile, model, effort, epoch, cwd)) = row else {
        return Err(fail("not_found", "Thread not found"));
    };
    let turn: Option<String> = tx
        .query_row(
            "SELECT id FROM turn WHERE thread_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1",
            [thread],
            |r| r.get(0),
        )
        .optional()?;
    let Some(turn) = turn else {
        return Err(fail(
            "not_found",
            "Thread has no Turn yet; its first request needs a Turn",
        ));
    };
    let (run, agents, instructions) = turn_row(tx, thread, &turn)?;
    let attachments: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM attachment_selection WHERE thread_id=?1)",
        [thread],
        |r| r.get(0),
    )?;
    Ok(Plan {
        source: "next",
        attempt: None,
        thread: thread.to_owned(),
        run,
        turn,
        profile,
        model,
        effort,
        epoch,
        cwd: Some(cwd),
        agents,
        instructions,
        history: History::Current,
        attachments_known_empty: !attachments,
        saved: None,
    })
}

/// One canonical input item, or a part saved state cannot determine.
enum Part {
    Known(Value),
    Unknown(String),
}
struct Assembled {
    plan: Plan,
    parts: Vec<Part>,
    /// The exact body when every part is known and the connection is configured.
    wire: Option<Value>,
    unknown: Vec<String>,
}

fn assemble(config: &Config, tx: &Connection, plan: Plan) -> Result<Assembled> {
    let mut parts = Vec::new();
    let mut unknown = Vec::new();
    let mut note = |parts: &mut Vec<Part>, reason: String| {
        unknown.push(reason.clone());
        parts.push(Part::Unknown(reason));
    };
    match &plan.instructions {
        Some(body) => {
            for item in leading_items(&plan.agents, body.as_deref())? {
                parts.push(Part::Known(item));
            }
        }
        None => {
            for item in leading_items(&plan.agents, None)? {
                parts.push(Part::Known(item));
            }
            note(
                &mut parts,
                "working instructions: this Turn has no saved instruction snapshot".into(),
            );
        }
    }
    let lease = match &plan.history {
        History::Current => Some(load_uncached(tx, &plan.thread, None, plan.epoch)?),
        History::Prefix(witness) => {
            let lease = load_uncached(tx, &plan.thread, Some(witness.nodes), plan.epoch)?;
            (lease.witness() == witness).then_some(lease)
        }
        History::Unknown(_) => None,
    };
    match (&plan.history, lease) {
        (_, Some(lease)) => {
            for item in lease.items() {
                if has_attachment_ref(item) {
                    note(
                        &mut parts,
                        "attachment content: bound from files at send time".into(),
                    );
                } else {
                    let mut item = item.clone();
                    strip_local(&mut item);
                    parts.push(Part::Known(item));
                }
            }
        }
        (History::Unknown(reason), None) => note(&mut parts, format!("history: {reason}")),
        (_, None) => note(
            &mut parts,
            "history: the saved path no longer begins with the nodes this attempt replayed".into(),
        ),
    }
    if !plan.attachments_known_empty {
        note(
            &mut parts,
            "attachment notices: depend on attachment state at send time".into(),
        );
    }
    match &plan.cwd {
        Some(cwd) => parts.push(Part::Known(context_item(&ContextLine {
            thread: &plan.thread,
            run: &plan.run,
            turn: &plan.turn,
            cwd: Path::new(cwd),
            profile: &plan.profile,
            model: &plan.model,
            effort: plan.effort.as_deref(),
            agents: &plan.agents,
        }))),
        None => note(
            &mut parts,
            "context line: this attempt's manifest does not record its cwd".into(),
        ),
    }
    let wire = if unknown.is_empty() {
        let input = parts
            .iter()
            .filter_map(|part| match part {
                Part::Known(item) => Some(item.clone()),
                Part::Unknown(_) => None,
            })
            .collect();
        let mut request =
            crate::model::response_request(&plan.model, input, plan.effort.as_deref(), true);
        crate::model::configure_shell_description(&mut request, config.shell.describe_calls);
        match config.connections.get(&plan.profile) {
            Some(connection) => {
                let client = HttpModelClient::for_connection(&plan.profile, connection.clone())
                    .map_err(|error| fail("config_invalid", error.message))?;
                let (wire, _session) = client
                    .wire_request(request, Some(&plan.thread))
                    .map_err(|error| fail("invalid_state", error.message))?;
                Some(wire)
            }
            None => {
                unknown.push(format!(
                    "wire form: connection {:?} is not in the current configuration",
                    plan.profile
                ));
                None
            }
        }
    } else {
        None
    };
    Ok(Assembled {
        plan,
        parts,
        wire,
        unknown,
    })
}

fn short(bytes: &[u8]) -> String {
    let mut hash = crate::digest::sha256_hex(bytes);
    hash.truncate(SHORT_HASH);
    hash
}
fn item_entry(index: usize, item: &Value) -> Value {
    let text = item.to_string();
    json!({
        "index": index,
        "type": item.get("type").and_then(Value::as_str).unwrap_or("message"),
        "role": item.get("role"),
        "chars": text.chars().count(),
        "sha256": short(text.as_bytes()),
    })
}
/// The conversation list inside a wire body, whatever the dialect calls it.
fn wire_items(wire: &Value) -> &[Value] {
    wire.get("input")
        .or_else(|| wire.get("messages"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

impl Assembled {
    /// Per-item hashes, from the wire body when known, otherwise canonical.
    fn item_hashes(&self) -> Vec<Option<String>> {
        match &self.wire {
            Some(wire) => wire_items(wire)
                .iter()
                .map(|item| Some(short(item.to_string().as_bytes())))
                .collect(),
            None => self
                .parts
                .iter()
                .map(|part| match part {
                    Part::Known(item) => Some(short(item.to_string().as_bytes())),
                    Part::Unknown(_) => None,
                })
                .collect(),
        }
    }
    fn describe(&self, body: bool, config: &Config) -> Value {
        let plan = &self.plan;
        let (form, items): (&str, Vec<Value>) = match &self.wire {
            Some(wire) => (
                "wire",
                wire_items(wire)
                    .iter()
                    .enumerate()
                    .map(|(index, item)| item_entry(index, item))
                    .collect(),
            ),
            None => (
                "canonical",
                self.parts
                    .iter()
                    .enumerate()
                    .map(|(index, part)| match part {
                        Part::Known(item) => item_entry(index, item),
                        Part::Unknown(reason) => json!({"index":index,"unknown":reason}),
                    })
                    .collect(),
            ),
        };
        let mut result = json!({
            "source": plan.source,
            "thread_id": plan.thread,
            "run_id": plan.run,
            "turn_id": plan.turn,
            "attempt_id": plan.attempt,
            "connection": plan.profile,
            "model": plan.model,
            "form": form,
            "items": items,
            "unknown": self.unknown,
        });
        if let Some(wire) = &self.wire {
            let bytes = serde_json::to_vec(wire).expect("JSON value serializes");
            result["total"] = json!({
                "sha256": crate::digest::sha256_hex(&bytes),
                "bytes": bytes.len(),
                "chars": String::from_utf8_lossy(&bytes).chars().count(),
            });
            result["fields"] = json!(
                wire.as_object()
                    .map(|object| object.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
            );
            if body {
                result["body"] = redacted(wire, config, &plan.profile);
            }
        } else {
            result["total"] = Value::Null;
        }
        if let Some(saved) = &plan.saved {
            let mut saved = saved.clone();
            if let Some(wire) = &self.wire {
                let bytes = serde_json::to_vec(wire)
                    .expect("JSON value serializes")
                    .len();
                saved["request_body_bytes_match"] = match saved["request_body_bytes"].as_u64() {
                    Some(recorded) => json!(recorded == bytes as u64),
                    None => Value::Null,
                };
            }
            result["saved"] = saved;
        }
        result
    }
}

/// extra_body values are the user's opaque configuration: hash them only.
fn redacted(wire: &Value, config: &Config, profile: &str) -> Value {
    let mut wire = wire.clone();
    if let (Some(object), Some(connection)) =
        (wire.as_object_mut(), config.connections.get(profile))
    {
        for key in connection.extra_body.keys() {
            if let Some(value) = object.get_mut(key) {
                *value = json!({"sha256": crate::digest::sha256_hex(value.to_string())});
            }
        }
    }
    wire
}

fn compare(primary: &Assembled, other: &Assembled) -> Value {
    let left = primary.item_hashes();
    let right = other.item_hashes();
    let common = left
        .iter()
        .zip(&right)
        .take_while(|(a, b)| a.is_some() && a == b)
        .count();
    let first_difference = (common < left.len().max(right.len())).then_some(common);
    json!({
        "attempt_id": other.plan.attempt,
        "items": right.len(),
        "common_prefix_items": common,
        "first_difference": first_difference,
        "total": other.wire.as_ref().map(|wire| {
            crate::digest::sha256_hex(serde_json::to_vec(wire).expect("JSON value serializes"))
        }),
        "unknown": other.unknown,
    })
}
