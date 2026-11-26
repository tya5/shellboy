//! Bounded projections of recorded work, made in one owner read transaction.
pub(crate) mod observation;
use crate::protocol::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, FixedOffset};
use rusqlite::{Connection, OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkItem {
    pub kind: String,
    pub id: String,
    pub state: String,
    pub thread_id: String,
    pub thread_title: String,
    pub parent_thread_id: Option<String>,
    pub current_connection: String,
    pub current_model: String,
    pub run_id: Option<String>,
    pub owner_run_id: Option<String>,
    pub turn_id: Option<String>,
    pub execution_id: Option<String>,
    pub message_id: Option<String>,
    pub attempt_id: Option<String>,
    pub operation_id: Option<String>,
    pub end_message_node_id: Option<String>,
    pub response_nodes: Vec<ResponseNodeRef>,
    pub response_nodes_truncated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expect_reply: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::protocol::deserialize_present_nullable"
    )]
    pub reply: Option<Option<crate::protocol::StatusReply>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub updated_at: Option<String>,
    pub activity_at: String,
    /// Read-time duration for live shells; saved duration for terminal shells.
    pub elapsed_ms: Option<u64>,
    pub last_output_at: Option<String>,
    #[serde(default)]
    pub stdin_open: Option<bool>,
    #[serde(default)]
    pub stdin_last_write_at: Option<String>,
    pub attempt_connection: Option<String>,
    pub attempt_model: Option<String>,
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instruction_warnings: Vec<InstructionWarning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_outcome: Option<crate::compaction::discard::CompactionOutcomeFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revisit: Option<RevisitSummary>,
    pub targets: Vec<crate::protocol::StatusTarget>,
    #[serde(default, flatten)]
    pub additional_properties: std::collections::BTreeMap<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResponseNodeRef {
    pub ordinal: u64,
    pub item_id: Option<String>,
    pub kind: String,
    pub phase: Option<String>,
    pub node_id: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatusFilters {
    pub thread_id: Option<String>,
    pub run_id: Option<String>,
    pub since: Option<String>,
    pub state: Vec<String>,
    pub all: bool,
    #[serde(default, flatten)]
    pub additional_properties: std::collections::BTreeMap<String, Value>,
}
/// Cumulative transport refusals and the corresponding owner budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IpcAdmissionStatus {
    pub max_connections: usize,
    pub wait: IpcAdmissionClass,
    pub ordinary: IpcAdmissionClass,
    pub stop: IpcAdmissionClass,
    pub status: IpcAdmissionClass,
    pub memory: IpcAdmissionClass,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IpcAdmissionClass {
    pub limit: usize,
    pub refused: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatusSnapshot {
    pub instance_id: String,
    pub observed_at: String,
    pub scope: String,
    pub consistency: String,
    pub fingerprint: String,
    #[serde(flatten)]
    pub page: RecordPage<WorkItem, StatusFilters>,
    pub receipt_budget: crate::protocol::ReceiptBudget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipc_admission: Option<IpcAdmissionStatus>,
    #[serde(default, flatten)]
    pub additional_properties: std::collections::BTreeMap<String, Value>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u32,
    instance: String,
    filter: String,
    scope: String,
    watermark: String,
    last: (String, String),
    last_activity: String,
}
type Result<T> = std::result::Result<T, ApiError>;
fn sql(_: rusqlite::Error) -> ApiError {
    crate::storage::save_error("Cannot read recorded work status")
}
fn cursor_error() -> ApiError {
    ApiError::new(
        ErrorCode::CursorExpired,
        "Work cursor is invalid or belongs to another query; restart without cursor",
    )
}
fn stamp(value: &str) -> Result<DateTime<FixedOffset>> {
    crate::protocol::rfc3339(value, || {
        ApiError::new(
            ErrorCode::InvalidState,
            "Recorded work timestamp is invalid",
        )
    })
}
// Both status paths project only bounded strings, never the full error record.
fn turn_recovery_fields(alias: &str) -> String {
    let saved = format!("{alias}.recovery_reason_json");
    ["message", "hint"].map(|key| format!(
        "CASE WHEN json_valid({saved}) THEN CASE WHEN json_type({saved},'$.{key}')='text' THEN substr(json_extract({saved},'$.{key}'),1,512) END END"
    )).join(",")
}
fn short(value: String, max: usize) -> String {
    value.chars().take(max).collect()
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum WorkKind {
    Attempt,
    Message,
    Run,
    Shell,
    Turn,
}
impl WorkKind {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "attempt" => Ok(Self::Attempt),
            "message" => Ok(Self::Message),
            "run" => Ok(Self::Run),
            "shell" => Ok(Self::Shell),
            "turn" => Ok(Self::Turn),
            _ => Err(ApiError::new(
                ErrorCode::InvalidState,
                "Unknown recorded work kind",
            )),
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Attempt => "attempt",
            Self::Message => "message",
            Self::Run => "run",
            Self::Shell => "shell",
            Self::Turn => "turn",
        }
    }
    fn terminal_states(self) -> &'static [&'static str] {
        match self {
            Self::Run | Self::Turn => &["completed", "stopped", "failed"],
            Self::Shell => &["exited", "failed", "stopped"],
            Self::Message => &["applied", "cancelled"],
            Self::Attempt => &["completed", "failed", "incomplete", "cancelled"],
        }
    }
}
struct PagePosition {
    activity: Vec<u8>,
    kind: WorkKind,
    id: String,
    watermark: Vec<u8>,
}

fn time_key(time: DateTime<FixedOffset>) -> Vec<u8> {
    let mut key = Vec::with_capacity(12);
    key.extend_from_slice(&((time.timestamp() as u64) ^ (1 << 63)).to_be_bytes());
    key.extend_from_slice(&time.timestamp_subsec_nanos().to_be_bytes());
    key
}

fn links(kind: WorkKind, item: &WorkItem) -> Result<Vec<Command>> {
    let mut links = match kind {
        WorkKind::Run => vec![Command::RunGet(RunTarget {
            run_id: item.id.clone(),
        })],
        WorkKind::Turn | WorkKind::Attempt => vec![Command::TurnGet(TurnTarget {
            turn_id: item.turn_id.clone().ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "Recorded work has no Turn membership",
                )
            })?,
        })],
        WorkKind::Shell => vec![
            Command::ShellGet(ExecutionTarget {
                execution_id: item.id.clone(),
            }),
            Command::ShellRead(ShellReadArgs {
                tail: false,
                segments: false,
                execution_id: item.id.clone(),
                cursor: None,
                encoding: None,
                limit_bytes: 16384,
            }),
        ],
        WorkKind::Message => vec![Command::MessageGet(MessageTarget {
            message_id: item.id.clone(),
        })],
    };
    if let Some(id) = &item.message_id {
        links.push(Command::MessageGet(MessageTarget {
            message_id: id.clone(),
        }));
    }
    if let Some(id) = &item.run_id {
        links.push(Command::RunGet(RunTarget { run_id: id.clone() }));
    }
    if let Some(id) = &item.turn_id {
        links.push(Command::TurnGet(TurnTarget {
            turn_id: id.clone(),
        }));
    }
    if let Some(id) = &item.execution_id {
        links.push(Command::ShellGet(ExecutionTarget {
            execution_id: id.clone(),
        }));
        links.push(Command::ShellRead(ShellReadArgs {
            tail: false,
            segments: false,
            execution_id: id.clone(),
            cursor: None,
            encoding: None,
            limit_bytes: 16384,
        }));
    }
    if let Some(id) = &item.end_message_node_id {
        links.push(Command::HistoryRead(HistoryReadArgs {
            end_message_only: false,
            parts: false,
            node_id: id.clone(),
            cursor: None,
            limit_bytes: 16384,
            encoding: "UTF-8".into(),
        }));
    }
    if let Some(id) = &item.operation_id {
        links.push(Command::OperationGet(OperationTarget {
            operation_id: id.clone(),
        }));
    }
    let mut unique = Vec::new();
    for link in links {
        if !unique.contains(&link) {
            unique.push(link);
        }
    }
    Ok(unique)
}

struct Scope {
    since: Option<DateTime<FixedOffset>>,
    states: Vec<String>,
    filter: String,
}
fn check_scope(db: &Connection, args: &StatusArgs) -> Result<Scope> {
    Command::Status(args.clone()).validate()?;
    if let Some(id) = &args.thread_id {
        let found: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1)",
                [id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if !found {
            return Err(ApiError::new(
                ErrorCode::ThreadNotFound,
                "Status Thread does not exist",
            ));
        }
    }
    if let Some(id) = &args.run_id {
        let thread: Option<String> = db
            .query_row("SELECT thread_id FROM run WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(sql)?;
        let Some(thread) = thread else {
            return Err(ApiError::new(
                ErrorCode::TargetUnavailable,
                "Status Run does not exist",
            ));
        };
        if args.thread_id.as_ref().is_some_and(|id| id != &thread) {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "The selected Run does not belong to the selected Thread",
            ));
        }
    }
    let since = args.since.as_deref().map(stamp).transpose()?;
    let mut states = args.state.clone();
    states.sort();
    states.dedup();
    let filter = serde_json::to_string(&(
        args.thread_id.clone(),
        args.run_id.clone(),
        since.map(|t| t.with_timezone(&chrono::Utc).to_rfc3339()),
        states.clone(),
        args.all,
    ))
    .expect("status filters");
    // Bind conditions without embedding up to 32 JSON-escaped state strings
    // in the opaque token. Valid filters must also produce bounded cursors.
    let filter = Sha256::digest(filter.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(Scope {
        since,
        states,
        filter,
    })
}
const RESPONSE_CANDIDATE_LIMIT: usize = 256;
const RESPONSE_NODE_LIMIT: usize = 64;
fn response_nodes(db: &Connection, attempt_id: &str) -> Result<(Vec<ResponseNodeRef>, bool)> {
    // Limit the PK range before node lookup/filtering. A candidate may be a
    // tool/reasoning/null node; candidates after the fixed window are unknown.
    let mut statement = db.prepare(
        "SELECT oi.ordinal,oi.item_id,oi.item_type,oi.phase,oi.node_id,n.kind FROM (SELECT ordinal,item_id,item_type,phase,node_id FROM output_item WHERE attempt_id=?1 ORDER BY ordinal LIMIT ?2) oi LEFT JOIN node n ON n.id=oi.node_id ORDER BY oi.ordinal",
    ).map_err(sql)?;
    let mut rows = statement
        .query(params![attempt_id, (RESPONSE_CANDIDATE_LIMIT + 1) as i64])
        .map_err(sql)?;
    let mut nodes = Vec::new();
    let mut candidates = 0;
    let mut truncated = false;
    while let Some(row) = rows.next().map_err(sql)? {
        candidates += 1;
        if candidates > RESPONSE_CANDIDATE_LIMIT {
            truncated = true;
            break;
        }
        let node_id: Option<String> = row.get(4).map_err(sql)?;
        let node_kind: Option<String> = row.get(5).map_err(sql)?;
        if node_kind.as_deref() != Some("assistant_response") {
            continue;
        }
        if nodes.len() == RESPONSE_NODE_LIMIT {
            truncated = true;
            break;
        }
        let Some(node_id) = node_id else { continue };
        nodes.push(ResponseNodeRef {
            ordinal: row.get::<_, i64>(0).map_err(sql)? as u64,
            item_id: row.get(1).map_err(sql)?,
            kind: row.get(2).map_err(sql)?,
            phase: row.get(3).map_err(sql)?,
            node_id,
        });
    }
    Ok((nodes, truncated))
}

fn decode_cursor(args: &StatusArgs, instance: &str, filter: &str) -> Result<Option<PagePosition>> {
    let cursor = if let Some(token) = &args.cursor {
        if token.len() > 8192 {
            return Err(cursor_error());
        }
        let bytes = URL_SAFE_NO_PAD.decode(token).map_err(|_| cursor_error())?;
        let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| cursor_error())?;
        if cursor.version != 3
            || cursor.instance != instance
            || cursor.scope != "durable_only"
            || cursor.filter != filter
        {
            return Err(cursor_error());
        }
        if cursor.last.1.is_empty() {
            return Err(cursor_error());
        }
        let watermark = URL_SAFE_NO_PAD
            .decode(cursor.watermark)
            .map_err(|_| cursor_error())?;
        if watermark.len() != 12 {
            return Err(cursor_error());
        }
        Some(PagePosition {
            activity: time_key(stamp(&cursor.last_activity).map_err(|_| cursor_error())?),
            kind: WorkKind::parse(&cursor.last.0).map_err(|_| cursor_error())?,
            id: cursor.last.1,
            watermark,
        })
    } else {
        None
    };
    Ok(cursor)
}
fn work_item(row: &rusqlite::Row<'_>, kind: WorkKind, state: String) -> Result<WorkItem> {
    let id: String = row.get(1).map_err(sql)?;
    let created_at: Option<String> = row.get(7).map_err(sql)?;
    let started_at: Option<String> = row.get(8).map_err(sql)?;
    let ended_at: Option<String> = row.get(9).map_err(sql)?;
    let updated_at: Option<String> = row.get(10).map_err(sql)?;
    let activity = [&created_at, &started_at, &ended_at, &updated_at]
        .into_iter()
        .filter_map(|v| v.as_deref())
        .map(|v| stamp(v).map(|t| (t, v)))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max_by_key(|(t, _)| *t)
        .ok_or_else(|| ApiError::new(ErrorCode::InvalidState, "Work has no recorded timestamp"))?;
    let mut item = WorkItem {
        kind: kind.as_str().into(),
        id: id.clone(),
        state,
        thread_id: row.get(3).map_err(sql)?,
        run_id: row.get(4).map_err(sql)?,
        turn_id: row.get(5).map_err(sql)?,
        owner_run_id: row.get(6).map_err(sql)?,
        created_at: created_at.clone(),
        started_at: started_at.clone(),
        ended_at: ended_at.clone(),
        updated_at: updated_at.clone(),
        activity_at: activity.1.into(),
        operation_id: row.get(11).map_err(sql)?,
        end_message_node_id: row.get(12).map_err(sql)?,
        expect_reply: false,
        reply: None,
        reply_to: None,
        attempt_connection: row
            .get::<_, Option<String>>(13)
            .map_err(sql)?
            .map(|s| short(s, 128)),
        attempt_model: row
            .get::<_, Option<String>>(14)
            .map_err(sql)?
            .map(|s| short(s, 256)),
        instruction_warnings: Vec::new(),
        compaction_outcome: None,
        reason: row
            .get::<_, Option<String>>(15)
            .map_err(sql)?
            .map(|s| short(s, 512)),
        message: row.get(16).map_err(sql)?,
        hint: row.get(17).map_err(sql)?,
        thread_title: short(row.get(18).map_err(sql)?, 80),
        parent_thread_id: row.get(19).map_err(sql)?,
        current_connection: short(row.get(20).map_err(sql)?, 128),
        current_model: short(row.get(21).map_err(sql)?, 256),
        execution_id: (kind == WorkKind::Shell).then_some(id.clone()),
        message_id: (kind == WorkKind::Message).then_some(id.clone()),
        attempt_id: (kind == WorkKind::Attempt).then_some(id.clone()),
        response_nodes: Vec::new(),
        response_nodes_truncated: false,
        elapsed_ms: None,
        last_output_at: None,
        stdin_open: None,
        stdin_last_write_at: None,
        revisit: None,
        targets: Vec::new(),
        additional_properties: Default::default(),
    };

    if kind == WorkKind::Message {
        let metadata: Option<String> = row.get(22).map_err(sql)?;
        if let Some(metadata) = metadata {
            let metadata: Value = serde_json::from_str(&metadata).map_err(|_| {
                ApiError::new(ErrorCode::InvalidState, "invalid message reply metadata")
            })?;
            item.expect_reply = metadata["expect_reply"] == true || metadata["expect_reply"] == 1;
            item.reply = metadata
                .get("reply")
                .filter(|value| !value.is_null())
                .and_then(crate::protocol::StatusReply::from_value)
                .map(Some);
            item.reply_to = metadata["reply_to"].as_str().map(str::to_owned);
        }
    }
    item.targets = links(kind, &item)?
        .into_iter()
        .filter_map(|command| {
            let value = serde_json::to_value(command).ok()?;
            Some(crate::protocol::StatusTarget {
                command: value.get("command")?.as_str()?.to_owned(),
                args: value.get("args")?.clone(),
                additional_properties: Default::default(),
            })
        })
        .collect();
    Ok(item)
}
fn encode_cursor(cursor: &Cursor) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).expect("status cursor"))
}

/// The caller supplies an already-open read transaction. No DB/filesystem/API
/// mutations, process liveness inspection, or other store observations occur.
pub fn snapshot(db: &Connection, args: &StatusArgs, instance: &str) -> Result<StatusSnapshot> {
    let scope = check_scope(db, args)?;
    let cursor = decode_cursor(args, instance, &scope.filter)?;
    let flags = rusqlite::functions::FunctionFlags::SQLITE_UTF8
        | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC;
    db.create_scalar_function("status_time_key", 1, flags, |ctx| {
        let time: Option<String> = ctx.get(0)?;
        time.as_deref()
            .map(stamp)
            .transpose()
            .map(|time| time.map(time_key).unwrap_or_default())
            .map_err(|err| {
                rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other(err.message)))
            })
    })
    .map_err(sql)?;
    let kinds = [
        WorkKind::Attempt,
        WorkKind::Message,
        WorkKind::Run,
        WorkKind::Shell,
        WorkKind::Turn,
    ];
    let sources = [
        (
            "a",
            "a.id",
            "model_attempt a JOIN turn t ON t.id=a.turn_id",
            "t.thread_id",
            "t.run_id",
            "a.started_at",
            "a.ended_at",
            "NULL",
        ),
        (
            "m",
            "m.message_node_id",
            "receipt m",
            "m.target_thread_id",
            "m.run_id",
            "m.created_at",
            "NULL",
            "NULLIF(m.updated_at,'')",
        ),
        (
            "r",
            "r.id",
            "run r",
            "r.thread_id",
            "r.id",
            "r.created_at",
            "r.ended_at",
            "NULL",
        ),
        (
            "s",
            "s.id",
            "shell_execution s JOIN run r ON r.id=s.owner_run_id",
            "r.thread_id",
            "s.owner_run_id",
            "s.started_at",
            "s.ended_at",
            "NULL",
        ),
        (
            "t",
            "t.id",
            "turn t",
            "t.thread_id",
            "t.run_id",
            "t.created_at",
            "t.ended_at",
            "NULL",
        ),
    ];
    let pending_only = scope.states.is_empty() && !args.all;
    let mut sources_sql = Vec::new();
    for (kind, (alias, id, from, thread, run, created, ended, updated)) in
        kinds.into_iter().zip(sources)
    {
        let mut predicate = String::new();
        if args.thread_id.is_some() {
            predicate.push_str(&format!(" AND {thread}=:thread"));
        }
        if args.run_id.is_some() {
            predicate.push_str(&format!(" AND {run}=:run"));
        }
        if !scope.states.is_empty() {
            predicate.push_str(&format!(
                " AND {alias}.state IN (SELECT value FROM json_each(:states))"
            ));
        } else if pending_only {
            predicate.push_str(&format!(
                " AND {alias}.state NOT IN ({})",
                kind.terminal_states()
                    .iter()
                    .map(|state| format!("'{state}'"))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        // Start at the partial index. Driving through the owner/Turn join first
        // would still walk terminal Runs and Turns before reaching pending work.
        let from = if pending_only {
            let (table, related) = from.split_once(" JOIN ").unwrap_or((from, ""));
            let mut indexed = format!("{table} INDEXED BY status_{}_pending", kind.as_str());
            if !related.is_empty() {
                indexed.push_str(" CROSS JOIN ");
                indexed.push_str(related);
            }
            indexed
        } else {
            from.to_owned()
        };
        let source = format!("status_time_key({created})");
        let activity = format!("max({source},status_time_key({ended}),status_time_key({updated}))");
        sources_sql.push(format!("SELECT '{}' kind,{id} id,{alias}.rowid rid,{source} source,{activity} activity FROM {from} WHERE 1{predicate}", kind.as_str()));
    }
    // Only sort/filter identity keys. The five detail branches seek by rowid
    // after the bounded page is chosen, so non-page work is never projected.
    let sources_sql = sources_sql.join(" UNION ALL ");
    let turn_recovery = turn_recovery_fields("t");
    let query = format!(
        r#"WITH input(thread,run,states,since,after,key_kind,key_id,watermark,lim) AS (
        VALUES (:thread,:run,:states,:since,:after,:kind,:id,:watermark,:lim)
    ), candidates AS NOT MATERIALIZED ({sources_sql}),
    page_keys AS MATERIALIZED (
        SELECT c.kind,c.id,c.rid,c.activity,
               coalesce(input.watermark,(SELECT max(source) FROM candidates WHERE activity>=:since)) watermark
        FROM candidates c CROSS JOIN input
        WHERE c.activity>=input.since AND (input.watermark IS NULL OR c.source<=input.watermark)
          AND (input.after IS NULL OR c.activity<input.after OR
               (c.activity=input.after AND (c.kind,c.id)>(input.key_kind,input.key_id)))
        ORDER BY c.activity DESC,c.kind,c.id LIMIT (SELECT lim FROM input)
    ),
    work(kind,id,state,thread_id,run_id,turn_id,owner_run_id,created_at,started_at,ended_at,updated_at,operation_id,answer_node_id,attempt_connection,attempt_model,reason,message,hint) AS (
      SELECT 'run',r.id,r.state,r.thread_id,r.id,(SELECT id FROM turn WHERE run_id=r.id ORDER BY created_at DESC,id DESC LIMIT 1),NULL,r.created_at,NULL,r.ended_at,NULL,NULL,(SELECT answer_node_id FROM turn WHERE run_id=r.id ORDER BY created_at DESC,id DESC LIMIT 1),NULL,NULL,r.stop_reason,NULL,NULL FROM run r WHERE r.rowid IN (SELECT rid FROM page_keys WHERE kind='run')
      UNION ALL SELECT 'turn',t.id,t.state,t.thread_id,t.run_id,t.id,NULL,t.created_at,NULL,t.ended_at,NULL,NULL,t.answer_node_id,NULL,NULL,CASE WHEN json_valid(t.recovery_reason_json) THEN COALESCE(json_extract(t.recovery_reason_json,'$.kind'),json_extract(t.recovery_reason_json,'$.code'),json_extract(t.recovery_reason_json,'$.reason')) END,{turn_recovery} FROM turn t JOIN run r ON r.id=t.run_id WHERE t.rowid IN (SELECT rid FROM page_keys WHERE kind='turn')
      UNION ALL SELECT 'shell',s.id,s.state,r.thread_id,s.owner_run_id,s.turn_id,s.owner_run_id,NULL,s.started_at,s.ended_at,NULL,NULL,NULL,NULL,NULL,s.end_reason,NULL,NULL FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE s.rowid IN (SELECT rid FROM page_keys WHERE kind='shell')
      UNION ALL SELECT 'message',m.message_node_id,m.state,m.target_thread_id,m.run_id,m.turn_id,NULL,m.created_at,NULL,NULL,NULLIF(m.updated_at,''),CASE WHEN EXISTS(SELECT 1 FROM operation_receipt o WHERE o.operation_id=m.client_operation_id) THEN m.client_operation_id END,NULL,NULL,NULL,m.hold_reason,NULL,NULL FROM receipt m LEFT JOIN run r ON r.id=m.run_id LEFT JOIN turn t ON t.id=m.turn_id WHERE m.rowid IN (SELECT rid FROM page_keys WHERE kind='message')
      UNION ALL SELECT 'attempt',a.id,a.state,t.thread_id,t.run_id,t.id,NULL,NULL,a.started_at,a.ended_at,NULL,NULL,t.answer_node_id,a.profile_id,a.model_id,CASE WHEN json_valid(a.error_json) THEN COALESCE(json_extract(a.error_json,'$.kind'),json_extract(a.error_json,'$.code')) END,NULL,NULL FROM model_attempt a JOIN turn t ON t.id=a.turn_id JOIN run r ON r.id=t.run_id WHERE a.rowid IN (SELECT rid FROM page_keys WHERE kind='attempt')
    ) SELECT w.*,t.title,t.parent_thread_id,t.profile_id,t.model_id,(SELECT json_object('expect_reply',json_extract(m.result_json,'$.expect_reply'),'reply',json_extract(m.result_json,'$.reply'),'reply_to',json_extract(m.result_json,'$.reply_to')) FROM receipt m WHERE w.kind='message' AND m.message_node_id=w.id),p.watermark FROM page_keys p JOIN work w ON w.kind=p.kind AND w.id=p.id JOIN thread t ON t.id=w.thread_id ORDER BY p.activity DESC,p.kind,p.id"#
    );
    let mut statement = db.prepare(&query).map_err(sql)?;
    let mut rows = statement
        .query(rusqlite::named_params! {
            ":thread": args.thread_id, ":run": args.run_id,
            ":states": serde_json::to_string(&scope.states).expect("status states"),
            ":since": scope.since.map(time_key).unwrap_or_default(),
            ":after": cursor.as_ref().map(|cursor| &cursor.activity),
            ":kind": cursor.as_ref().map(|cursor| cursor.kind.as_str()),
            ":id": cursor.as_ref().map(|cursor| &cursor.id),
            ":watermark": cursor.as_ref().map(|cursor| &cursor.watermark),
            ":lim": i64::from(args.limit) + 1,
        })
        .map_err(sql)?;
    let mut selected = Vec::new();
    let mut watermark: Option<Vec<u8>> = None;
    while let Some(row) = rows.next().map_err(sql)? {
        let kind = WorkKind::parse(&row.get::<_, String>(0).map_err(sql)?)?;
        let mut item = work_item(row, kind, row.get(2).map_err(sql)?)?;
        if kind == WorkKind::Run {
            item.revisit = crate::control::revisit::summary_for_run(db, &item.id).map_err(sql)?;
        }
        watermark = row.get(23).map_err(sql)?;
        selected.push(item);
    }
    // A page can hold many Runs; bound the added warning bytes without hiding
    // any Run's warnings. The ordinary cursor resumes at the first deferred row.
    let mut remaining_warning_bytes = 256_usize * 1024;
    let mut page_len = 0;
    for projected in selected.iter_mut().take(args.limit as usize) {
        if projected.kind == "run" {
            let warnings: Option<String> = db
                .query_row(
                    "SELECT instruction_warnings_json FROM run WHERE id=?1",
                    [&projected.id],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            if let Some(warnings) = warnings {
                let Some(remaining) = remaining_warning_bytes.checked_sub(warnings.len()) else {
                    break;
                };
                remaining_warning_bytes = remaining;
                projected.instruction_warnings =
                    crate::instructions::decode_warnings(Some(&warnings));
            }
        }
        page_len += 1;
    }
    let next_cursor = if selected.len() > page_len {
        selected.truncate(page_len);
        let last = selected.last().expect("nonzero page");
        Some(encode_cursor(&Cursor {
            version: 3,
            instance: instance.into(),
            scope: "durable_only".into(),
            filter: scope.filter.clone(),
            watermark: URL_SAFE_NO_PAD.encode(watermark.expect("nonempty page source timestamp")),
            last: (last.kind.clone(), last.id.clone()),
            last_activity: last.activity_at.clone(),
        }))
    } else {
        None
    };
    for projected in &mut selected {
        if projected.kind == "attempt" {
            let query = format!(
                "SELECT {} FROM model_attempt r WHERE r.id=?1",
                crate::compaction::discard::status_sql("r")
            );
            let text: Option<String> = db
                .query_row(&query, [&projected.id], |r| r.get(0))
                .map_err(sql)?;
            (projected.response_nodes, projected.response_nodes_truncated) =
                response_nodes(db, &projected.id)?;
            projected.compaction_outcome =
                text.as_deref().and_then(crate::compaction::discard::decode);
        }
    }
    let mut digest = Sha256::new();
    digest.update(scope.filter.as_bytes());
    digest.update(args.limit.to_le_bytes());
    for item in &selected {
        let body = serde_json::to_vec(&serde_json::json!({
            "kind": item.kind, "id": item.id, "state": item.state,
            "thread_id": item.thread_id, "parent_thread_id": item.parent_thread_id,
            "run_id": item.run_id, "owner_run_id": item.owner_run_id, "turn_id": item.turn_id,
            "execution_id": item.execution_id, "message_id": item.message_id,
            "attempt_id": item.attempt_id, "operation_id": item.operation_id,
            "end_message_node_id": item.end_message_node_id, "reply_to": item.reply_to,
            "expect_reply": item.expect_reply,
            "created_at": item.created_at, "started_at": item.started_at,
            "ended_at": item.ended_at, "updated_at": item.updated_at,
            "response_nodes": item.response_nodes, "response_nodes_truncated": item.response_nodes_truncated,
        })).expect("durable work facts");
        digest.update((body.len() as u64).to_le_bytes());
        digest.update(&body);
    }
    let fingerprint = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let filters = StatusFilters {
        thread_id: args.thread_id.clone(),
        run_id: args.run_id.clone(),
        since: scope
            .since
            .map(|time| time.with_timezone(&chrono::Utc).to_rfc3339()),
        state: scope.states.clone(),
        all: args.all,
        additional_properties: Default::default(),
    };
    // Enrich only the bounded result page. Dynamic observations never enter
    // the durable fingerprint or cursor sort keys.
    let observed = chrono::Utc::now();
    for selected in &mut selected {
        if selected.kind == "shell" {
            let timing = crate::shell::timing::read(db, &selected.id, observed).map_err(sql)?;
            (selected.stdin_open, selected.stdin_last_write_at) =
                crate::stdin_facts::read(db, &selected.id).map_err(sql)?;
            selected.elapsed_ms = timing.elapsed_ms;
            selected.last_output_at = timing.last_output_at;
        }
    }
    Ok(StatusSnapshot {
        ipc_admission: None,
        receipt_budget: serde_json::from_value(crate::control::receipt::summary(db, instance)?)
            .map_err(|_| ApiError::new(ErrorCode::InvalidState, "invalid receipt summary"))?,
        page: RecordPage::new(args.limit, filters, selected, next_cursor),
        instance_id: instance.into(),
        observed_at: observed.to_rfc3339(),
        scope: "durable_only".into(),
        consistency: "single_sqlite_read_transaction".into(),
        fingerprint,
        additional_properties: Default::default(),
    })
}

#[cfg(test)]
mod response_node_tests {
    use super::*;

    #[test]
    fn status_projects_only_the_returned_page_and_ignores_terminal_work_by_default() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','Title','/','','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('live','t','live','2026-11-24T00:00:00.000000002Z'),('older','t','completed','2026-11-23T19:00:00.000000001-05:00');
            WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<5000)
            INSERT INTO run(id,thread_id,state,created_at) SELECT 'history'||i,'t','completed','2026-11-22T00:00:00Z' FROM n;").unwrap();
        let args = StatusArgs {
            limit: 1,
            ..Default::default()
        };
        let active = snapshot(&db, &args, "fixture").unwrap();
        assert_eq!(active.page.items.len(), 1);
        assert_eq!(active.page.items[0].id, "live");
        assert!(active.page.next_cursor.is_none());
        let all = StatusArgs { all: true, ..args };
        let first = snapshot(&db, &all, "fixture").unwrap();
        db.execute(
            "UPDATE run SET stop_reason='changed off page' WHERE id='older'",
            [],
        )
        .unwrap();
        db.execute(
            "UPDATE run SET stop_reason='on page but not hashed' WHERE id='live'",
            [],
        )
        .unwrap();
        let unchanged = snapshot(&db, &all, "fixture").unwrap();
        assert_eq!(
            unchanged.page.items[0].reason.as_deref(),
            Some("on page but not hashed")
        );
        assert_eq!(unchanged.fingerprint, first.fingerprint);
        let next = snapshot(
            &db,
            &StatusArgs {
                cursor: first.page.next_cursor,
                ..all
            },
            "fixture",
        )
        .unwrap();
        assert_eq!(next.page.items.len(), 1);
        assert_eq!(next.page.items[0].id, "older");
        assert_eq!(
            next.page.items[0].reason.as_deref(),
            Some("changed off page")
        );
    }

    #[test]
    fn default_status_page_ignores_terminal_history() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('current','/','','p','m','2026-11-24T00:00:00Z');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('pending','current','live','2026-11-24T00:00:00Z');").unwrap();
        let page = |args: &StatusArgs| {
            let result = snapshot(&db, args, "fixture").unwrap();
            assert_eq!(
                result
                    .page
                    .items
                    .iter()
                    .map(|item| item.id.as_str())
                    .collect::<Vec<_>>(),
                ["pending"]
            );
            assert!(result.page.next_cursor.is_none());
        };
        let global = StatusArgs::default();
        let scoped = StatusArgs {
            thread_id: Some("current".into()),
            ..Default::default()
        };
        page(&global);
        page(&scoped);
        db.execute_batch("CREATE TEMP TABLE history_fixture AS
            WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2000)
            SELECT printf('history-%05d',i) id, CASE WHEN i%2=0 THEN 'current' ELSE printf('unrelated-%05d',i) END thread_id, '2026-11-23T00:00:00Z' at FROM n;
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)
                SELECT thread_id,'/','','p','m',at FROM history_fixture WHERE thread_id<>'current';
            INSERT INTO run(id,thread_id,state,created_at) SELECT id,thread_id,'completed',at FROM history_fixture;
            INSERT INTO turn(id,thread_id,run_id,state,created_at) SELECT id,thread_id,id,'completed',at FROM history_fixture;
            INSERT INTO model_attempt(id,turn_id,kind,thread_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)
                SELECT id,id,'conversation',thread_id,0,0,'p','m',0,'completed','{}',at FROM history_fixture;
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)
                SELECT id,id,id,id,'exited','/','{}','UTF-8','UTF-8','closed',at FROM history_fixture;
            INSERT INTO node(id,kind,created_at,payload_json) SELECT id,'user',at,'{}' FROM history_fixture;
            INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)
                SELECT id,id,'null',thread_id,id,'applied',at FROM history_fixture;").unwrap();
        page(&global);
        page(&scoped);
    }

    #[test]
    fn response_nodes_bound_candidates_and_disclose_incomplete_results() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE node(id TEXT PRIMARY KEY, kind TEXT NOT NULL);
             CREATE TABLE thread_path(thread_id TEXT NOT NULL,ordinal INTEGER NOT NULL,node_id TEXT NOT NULL,PRIMARY KEY(thread_id,ordinal),UNIQUE(thread_id,node_id));
             CREATE TABLE output_item(attempt_id TEXT NOT NULL, ordinal INTEGER NOT NULL, item_id TEXT, item_type TEXT NOT NULL, phase TEXT, node_id TEXT, PRIMARY KEY(attempt_id,ordinal));",
        ).unwrap();
        db.execute("INSERT INTO node VALUES ('commentary-node','assistant_response'),('final-node','assistant_response'),('user-node','user_message')", []).unwrap();
        db.execute("INSERT INTO thread_path VALUES ('thread',0,'commentary-node'),('thread',1,'final-node')", []).unwrap();
        db.execute("INSERT INTO output_item VALUES ('attempt',0,'item-c','message','commentary','commentary-node'),('attempt',1,'item-c-part','message','commentary','commentary-node'),('attempt',2,'item-f','message','final','final-node'),('attempt',3,'item-u','message','final','user-node'),('attempt',4,'item-null','message','final',NULL)", []).unwrap();

        let (nodes, truncated) = response_nodes(&db, "attempt").unwrap();
        assert!(!truncated);
        assert_eq!(
            nodes.len(),
            3,
            "preserve each output item, including duplicate node links"
        );
        assert_eq!(nodes[0].node_id, "commentary-node");
        assert_eq!(nodes[0].kind, "message");
        assert_eq!(nodes[0].phase.as_deref(), Some("commentary"));
        assert_eq!(nodes[1].node_id, "commentary-node");
        assert_eq!(nodes[1].item_id.as_deref(), Some("item-c-part"));
        assert_eq!(nodes[2].node_id, "final-node");
        assert_eq!(nodes[2].phase.as_deref(), Some("final"));
        for item in &nodes {
            let ordinal: i64 = db
                .query_row(
                    "SELECT ordinal FROM thread_path WHERE thread_id='thread' AND node_id=?1",
                    [&item.node_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                ordinal >= 0,
                "every published ID is a history.window anchor"
            );
        }

        // No rows is a complete empty list; empty plus truncated below is explicitly unknown.
        let (none, truncated) = response_nodes(&db, "pre-save-or-failed").unwrap();
        assert!(none.is_empty());
        assert!(!truncated);

        // 256 noneligible rows come first, followed by eligible nodes: the
        // eligible tail must not cause an unbounded scan, and [] is not absence.
        for ordinal in 0..RESPONSE_CANDIDATE_LIMIT as i64 {
            db.execute(
                "INSERT INTO output_item VALUES ('prefix',?1,NULL,'function_call','tool',NULL)",
                [ordinal],
            )
            .unwrap();
        }
        for ordinal in RESPONSE_CANDIDATE_LIMIT as i64..(RESPONSE_CANDIDATE_LIMIT as i64 + 10) {
            let id = format!("tail-{ordinal}");
            db.execute("INSERT INTO node VALUES (?1,'assistant_response')", [&id])
                .unwrap();
            db.execute(
                "INSERT INTO output_item VALUES ('prefix',?1,NULL,'message','final',?2)",
                params![ordinal, id],
            )
            .unwrap();
        }
        let (nodes, truncated) = response_nodes(&db, "prefix").unwrap();
        assert!(nodes.is_empty());
        assert!(
            truncated,
            "unexamined candidate tail makes an empty result incomplete"
        );

        // Boundary: exactly the candidate cap is complete; cap+1 is unknown.
        for ordinal in 0..RESPONSE_CANDIDATE_LIMIT as i64 {
            db.execute(
                "INSERT INTO output_item VALUES ('boundary',?1,NULL,'function_call','tool',NULL)",
                [ordinal],
            )
            .unwrap();
        }
        let (nodes, truncated) = response_nodes(&db, "boundary").unwrap();
        assert!(nodes.is_empty());
        assert!(!truncated);
        db.execute(
            "INSERT INTO output_item VALUES ('boundary',?1,NULL,'function_call','tool',NULL)",
            [RESPONSE_CANDIDATE_LIMIT as i64],
        )
        .unwrap();
        let (nodes, truncated) = response_nodes(&db, "boundary").unwrap();
        assert!(nodes.is_empty());
        assert!(truncated);
        let plan: Vec<String> = {
            let mut statement = db.prepare("EXPLAIN QUERY PLAN SELECT ordinal,item_id,item_type,phase,node_id FROM output_item WHERE attempt_id=?1 ORDER BY ordinal LIMIT ?2").unwrap();
            let rows = statement
                .query_map(
                    params!["boundary", (RESPONSE_CANDIDATE_LIMIT + 1) as i64],
                    |row| row.get(3),
                )
                .unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        let plan = plan.join(" | ");
        assert!(
            plan.contains("output_item_1") && plan.contains("attempt_id=?"),
            "expected PK range scan: {plan}"
        );
    }
}
