//! Bounded projections of recorded work, made in one owner read transaction.
use crate::protocol::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, FixedOffset};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub answer_node_id: Option<String>,
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
    pub targets: Vec<Command>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub instance_id: String,
    pub observed_at: String,
    pub scope: String,
    pub consistency: String,
    pub fingerprint: String,
    pub filters: StatusArgs,
    pub items: Vec<WorkItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub receipt_budget: Value,
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
    if let Some(id) = &item.answer_node_id {
        links.push(Command::HistoryGet(HistoryGetArgs {
            final_answer_only: false,
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
        answer_node_id: row.get(12).map_err(sql)?,
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
        elapsed_ms: None,
        last_output_at: None,
        revisit: None,
        targets: Vec::new(),
    };
    item.targets = links(kind, &item)?;
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
      SELECT 'run',r.id,r.state,r.thread_id,r.id,(SELECT id FROM turn WHERE run_id=r.id ORDER BY created_at DESC,id DESC LIMIT 1),r.owner_run_id,r.created_at,NULL,r.ended_at,NULL,NULL,(SELECT answer_node_id FROM turn WHERE run_id=r.id ORDER BY created_at DESC,id DESC LIMIT 1),NULL,NULL,r.stop_reason FROM run r
      UNION ALL SELECT 'turn',t.id,t.state,t.thread_id,t.run_id,t.id,r.owner_run_id,t.created_at,NULL,t.ended_at,NULL,NULL,t.answer_node_id,NULL,NULL,CASE WHEN json_valid(t.recovery_reason_json) THEN COALESCE(json_extract(t.recovery_reason_json,'$.kind'),json_extract(t.recovery_reason_json,'$.code')) END FROM turn t JOIN run r ON r.id=t.run_id
      UNION ALL SELECT 'shell',s.id,s.state,r.thread_id,s.owner_run_id,s.turn_id,r.owner_run_id,NULL,s.started_at,s.ended_at,NULL,NULL,NULL,NULL,NULL,s.end_reason FROM shell_execution s JOIN run r ON r.id=s.owner_run_id
      UNION ALL SELECT 'message',m.message_node_id,m.state,m.target_thread_id,m.run_id,m.turn_id,r.owner_run_id,m.created_at,NULL,NULL,NULLIF(m.updated_at,''),CASE WHEN EXISTS(SELECT 1 FROM operation_receipt o WHERE o.operation_id=m.client_operation_id) THEN m.client_operation_id END,t.answer_node_id,NULL,NULL,m.hold_reason FROM receipt m LEFT JOIN run r ON r.id=m.run_id LEFT JOIN turn t ON t.id=m.turn_id
      UNION ALL SELECT 'attempt',a.id,a.state,t.thread_id,t.run_id,t.id,r.owner_run_id,NULL,a.started_at,a.ended_at,NULL,NULL,t.answer_node_id,a.profile_id,a.model_id,CASE WHEN json_valid(a.error_json) THEN COALESCE(json_extract(a.error_json,'$.kind'),json_extract(a.error_json,'$.code')) END FROM model_attempt a JOIN turn t ON t.id=a.turn_id JOIN run r ON r.id=t.run_id
    ) SELECT w.*,t.title,t.parent_thread_id,t.profile_id,t.model_id FROM work w JOIN thread t ON t.id=w.thread_id WHERE (?1 IS NULL OR w.thread_id=?1) AND (?2 IS NULL OR w.run_id=?2) ORDER BY w.kind,w.id";
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
            projected.item.revisit = crate::control::revisit::summary(db, &projected.item.id)?;
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
    let mut filters = args.clone();
    filters.cursor = None;
    filters.state = scope.states;
    // Enrich only the bounded result page. Dynamic observations never enter
    // the durable fingerprint or cursor sort keys.
    let observed = chrono::Utc::now();
    for selected in &mut selected {
        if selected.item.kind == "shell" {
            let timing =
                crate::shell::timing::read(db, &selected.item.id, observed).map_err(sql)?;
            selected.item.elapsed_ms = timing.elapsed_ms;
            selected.item.last_output_at = timing.last_output_at;
        }
    }
    Ok(StatusSnapshot {
        receipt_budget: crate::control::receipt::summary(db, instance)?,
        filters,
        instance_id: instance.into(),
        observed_at: observed.to_rfc3339(),
        scope: "durable_only".into(),
        consistency: "single_sqlite_read_transaction".into(),
        fingerprint,
        items: selected.into_iter().map(|p| p.item).collect(),
        next_cursor,
    })
}
