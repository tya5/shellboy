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
use std::cmp::Ordering;

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
    pub attempt_connection: Option<String>,
    pub attempt_model: Option<String>,
    pub reason: Option<String>,
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
    pub memory: bool,
    pub thread_id: Option<String>,
    pub run_id: Option<String>,
    pub since: Option<String>,
    pub state: Vec<String>,
    pub all: bool,
    pub limit: u32,
    pub cursor: Option<String>,
    #[serde(default, flatten)]
    pub additional_properties: std::collections::BTreeMap<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatusSnapshot {
    pub instance_id: String,
    pub observed_at: String,
    pub scope: String,
    pub consistency: String,
    pub fingerprint: String,
    pub filters: StatusFilters,
    pub items: Vec<WorkItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub receipt_budget: crate::protocol::ReceiptBudget,
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
    fn terminal(self, state: &str) -> bool {
        match self {
            Self::Run | Self::Turn => matches!(state, "completed" | "stopped" | "failed"),
            Self::Shell => matches!(state, "exited" | "failed" | "stopped"),
            Self::Message => matches!(state, "applied" | "cancelled"),
            Self::Attempt => matches!(state, "completed" | "failed" | "incomplete" | "cancelled"),
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
struct SortKey {
    activity: DateTime<FixedOffset>,
    kind: WorkKind,
    id: String,
}
impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .activity
            .cmp(&self.activity)
            .then_with(|| self.kind.cmp(&other.kind))
            .then_with(|| self.id.cmp(&other.id))
    }
}
impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
struct ProjectedWork {
    item: WorkItem,
    key: SortKey,
    source_at: DateTime<FixedOffset>,
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
            execution_id: id.clone(),
            cursor: None,
            encoding: None,
            limit_bytes: 16384,
        }));
    }
    if let Some(id) = &item.end_message_node_id {
        links.push(Command::HistoryGet(HistoryGetArgs {
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
struct PagePosition {
    after: SortKey,
    watermark: DateTime<FixedOffset>,
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
        if cursor.version != 2
            || cursor.instance != instance
            || cursor.scope != "durable_only"
            || cursor.filter != filter
        {
            return Err(cursor_error());
        }
        if cursor.last.1.is_empty() {
            return Err(cursor_error());
        }
        Some(PagePosition {
            after: SortKey {
                activity: stamp(&cursor.last_activity).map_err(|_| cursor_error())?,
                kind: WorkKind::parse(&cursor.last.0).map_err(|_| cursor_error())?,
                id: cursor.last.1,
            },
            watermark: stamp(&cursor.watermark).map_err(|_| cursor_error())?,
        })
    } else {
        None
    };
    Ok(cursor)
}
fn work_item(row: &rusqlite::Row<'_>, kind: WorkKind, state: String) -> Result<ProjectedWork> {
    let id: String = row.get(1).map_err(sql)?;
    let created_at: Option<String> = row.get(7).map_err(sql)?;
    let started_at: Option<String> = row.get(8).map_err(sql)?;
    let ended_at: Option<String> = row.get(9).map_err(sql)?;
    let updated_at: Option<String> = row.get(10).map_err(sql)?;
    let source_at = stamp(
        created_at
            .as_deref()
            .or(started_at.as_deref())
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "Work has no recorded source timestamp",
                )
            })?,
    )?;
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
        reason: row
            .get::<_, Option<String>>(15)
            .map_err(sql)?
            .map(|s| short(s, 512)),
        thread_title: short(row.get(16).map_err(sql)?, 80),
        parent_thread_id: row.get(17).map_err(sql)?,
        current_connection: short(row.get(18).map_err(sql)?, 128),
        current_model: short(row.get(19).map_err(sql)?, 256),
        execution_id: (kind == WorkKind::Shell).then_some(id.clone()),
        message_id: (kind == WorkKind::Message).then_some(id.clone()),
        attempt_id: (kind == WorkKind::Attempt).then_some(id.clone()),
        response_nodes: Vec::new(),
        response_nodes_truncated: false,
        elapsed_ms: None,
        last_output_at: None,
        revisit: None,
        targets: Vec::new(),
        additional_properties: Default::default(),
    };

    if kind == WorkKind::Message {
        let metadata: Option<String> = row.get(20).map_err(sql)?;
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
    let key = SortKey {
        activity: activity.0,
        kind,
        id,
    };
    Ok(ProjectedWork {
        item,
        key,
        source_at,
    })
}
fn encode_cursor(cursor: &Cursor) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).expect("status cursor"))
}

/// The caller supplies an already-open read transaction. No DB/filesystem/API
/// mutations, process liveness inspection, or other store observations occur.
pub fn snapshot(db: &Connection, args: &StatusArgs, instance: &str) -> Result<StatusSnapshot> {
    let scope = check_scope(db, args)?;
    let cursor = decode_cursor(args, instance, &scope.filter)?;
    let mut watermark = cursor.as_ref().map(|position| position.watermark);
    // Every source keeps its native lifecycle. Only common metadata and explicit
    // identifiers are joined; origin tokens and model/shell payloads stay out.
    let query="WITH work(kind,id,state,thread_id,run_id,turn_id,owner_run_id,created_at,started_at,ended_at,updated_at,operation_id,answer_node_id,attempt_connection,attempt_model,reason) AS (
      SELECT 'run',r.id,r.state,r.thread_id,r.id,(SELECT id FROM turn WHERE run_id=r.id ORDER BY created_at DESC,id DESC LIMIT 1),NULL,r.created_at,NULL,r.ended_at,NULL,NULL,(SELECT answer_node_id FROM turn WHERE run_id=r.id ORDER BY created_at DESC,id DESC LIMIT 1),NULL,NULL,r.stop_reason FROM run r
      UNION ALL SELECT 'turn',t.id,t.state,t.thread_id,t.run_id,t.id,NULL,t.created_at,NULL,t.ended_at,NULL,NULL,t.answer_node_id,NULL,NULL,CASE WHEN json_valid(t.recovery_reason_json) THEN COALESCE(json_extract(t.recovery_reason_json,'$.kind'),json_extract(t.recovery_reason_json,'$.code')) END FROM turn t JOIN run r ON r.id=t.run_id
      UNION ALL SELECT 'shell',s.id,s.state,r.thread_id,s.owner_run_id,s.turn_id,s.owner_run_id,NULL,s.started_at,s.ended_at,NULL,NULL,NULL,NULL,NULL,s.end_reason FROM shell_execution s JOIN run r ON r.id=s.owner_run_id
      UNION ALL SELECT 'message',m.message_node_id,m.state,m.target_thread_id,m.run_id,m.turn_id,NULL,m.created_at,NULL,NULL,NULLIF(m.updated_at,''),CASE WHEN EXISTS(SELECT 1 FROM operation_receipt o WHERE o.operation_id=m.client_operation_id) THEN m.client_operation_id END,NULL,NULL,NULL,m.hold_reason FROM receipt m LEFT JOIN run r ON r.id=m.run_id LEFT JOIN turn t ON t.id=m.turn_id
      UNION ALL SELECT 'attempt',a.id,a.state,t.thread_id,t.run_id,t.id,NULL,NULL,a.started_at,a.ended_at,NULL,NULL,t.answer_node_id,a.profile_id,a.model_id,CASE WHEN json_valid(a.error_json) THEN COALESCE(json_extract(a.error_json,'$.kind'),json_extract(a.error_json,'$.code')) END FROM model_attempt a JOIN turn t ON t.id=a.turn_id JOIN run r ON r.id=t.run_id
    ) SELECT w.*,t.title,t.parent_thread_id,t.profile_id,t.model_id,(SELECT json_object('expect_reply',json_extract(m.result_json,'$.expect_reply'),'reply',json_extract(m.result_json,'$.reply'),'reply_to',json_extract(m.result_json,'$.reply_to')) FROM receipt m WHERE w.kind='message' AND m.message_node_id=w.id) FROM work w JOIN thread t ON t.id=w.thread_id WHERE (?1 IS NULL OR w.thread_id=?1) AND (?2 IS NULL OR w.run_id=?2) ORDER BY w.kind,w.id";
    let mut statement = db.prepare(query).map_err(sql)?;
    let mut rows = statement
        .query(params![args.thread_id, args.run_id])
        .map_err(sql)?;
    let mut digest = Sha256::new();
    let mut selected = Vec::new();
    while let Some(row) = rows.next().map_err(sql)? {
        let kind = WorkKind::parse(&row.get::<_, String>(0).map_err(sql)?)?;
        let state: String = row.get(2).map_err(sql)?;
        // Keep native-state exclusion ahead of timestamp/target projection, as
        // terminal rows can dominate the durable store in the default view.
        if (scope.states.is_empty() && !args.all && kind.terminal(&state))
            || (!scope.states.is_empty() && !scope.states.contains(&state))
        {
            continue;
        }
        let mut projected = work_item(row, kind, state)?;
        if kind == WorkKind::Run {
            projected.item.revisit =
                crate::control::revisit::summary_for_run(db, &projected.item.id).map_err(sql)?;
        }
        let item = &projected.item;
        if scope
            .since
            .is_some_and(|since| projected.key.activity < since)
        {
            continue;
        }
        if cursor.is_some() && watermark.is_some_and(|upper| projected.source_at > upper) {
            continue;
        }
        if cursor.is_none() {
            watermark =
                Some(watermark.map_or(projected.source_at, |upper| upper.max(projected.source_at)));
        }
        let body = serde_json::to_vec(&item).expect("work item");
        digest.update((body.len() as u64).to_le_bytes());
        digest.update(&body);
        if cursor
            .as_ref()
            .is_some_and(|position| projected.key <= position.after)
        {
            continue;
        }
        selected.push(projected);
        selected.sort_by(|a, b| a.key.cmp(&b.key));
        selected.truncate(args.limit as usize + 1);
    }
    let fingerprint = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let next_cursor = if selected.len() > args.limit as usize {
        selected.pop();
        let last = &selected.last().expect("nonzero page").item;
        Some(encode_cursor(&Cursor {
            version: 2,
            instance: instance.into(),
            scope: "durable_only".into(),
            filter: scope.filter,
            watermark: watermark
                .expect("nonempty page source timestamp")
                .to_rfc3339(),
            last: (last.kind.clone(), last.id.clone()),
            last_activity: last.activity_at.clone(),
        }))
    } else {
        None
    };
    let filters = StatusFilters {
        memory: args.memory,
        thread_id: args.thread_id.clone(),
        run_id: args.run_id.clone(),
        since: args.since.clone(),
        state: scope.states.clone(),
        all: args.all,
        limit: args.limit,
        cursor: None,
        additional_properties: Default::default(),
    };
    // Enrich only the bounded result page. Dynamic observations never enter
    // the durable fingerprint or cursor sort keys.
    let observed = chrono::Utc::now();
    for selected in &mut selected {
        if selected.item.kind == "attempt" {
            (
                selected.item.response_nodes,
                selected.item.response_nodes_truncated,
            ) = response_nodes(db, &selected.item.id)?;
        }
        if selected.item.kind == "shell" {
            let timing =
                crate::shell::timing::read(db, &selected.item.id, observed).map_err(sql)?;
            selected.item.elapsed_ms = timing.elapsed_ms;
            selected.item.last_output_at = timing.last_output_at;
        }
    }
    Ok(StatusSnapshot {
        receipt_budget: serde_json::from_value(crate::control::receipt::summary(db, instance)?)
            .map_err(|_| ApiError::new(ErrorCode::InvalidState, "invalid receipt summary"))?,
        filters,
        instance_id: instance.into(),
        observed_at: observed.to_rfc3339(),
        scope: "durable_only".into(),
        consistency: "single_sqlite_read_transaction".into(),
        fingerprint,
        items: selected.into_iter().map(|p| p.item).collect(),
        next_cursor,
        additional_properties: Default::default(),
    })
}

#[cfg(test)]
mod response_node_tests {
    use super::*;

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

        // Count VM instructions for the complete production lookup (candidate
        // read plus node joins/filter). Growing rows past the LIMIT must not add work.
        let steps = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let measured = steps.clone();
        db.progress_handler(
            1,
            Some(move || {
                measured.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
        let (_, truncated) = response_nodes(&db, "prefix").unwrap();
        assert!(truncated);
        let bounded_steps = steps.swap(0, std::sync::atomic::Ordering::Relaxed);
        for ordinal in 266..100_266 {
            db.execute(
                "INSERT INTO output_item VALUES ('prefix',?1,NULL,'function_call','tool',NULL)",
                [ordinal],
            )
            .unwrap();
        }
        steps.store(0, std::sync::atomic::Ordering::Relaxed);
        let (_, truncated) = response_nodes(&db, "prefix").unwrap();
        assert!(truncated);
        let large_steps = steps.load(std::sync::atomic::Ordering::Relaxed);
        db.progress_handler(0, None::<fn() -> bool>).unwrap();
        assert_eq!(
            large_steps, bounded_steps,
            "actual lookup VM steps must remain fixed after the 256-candidate prefix despite 100k later rows"
        );
    }
}
