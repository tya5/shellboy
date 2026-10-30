//! Immutable conversation nodes and exact, versioned Thread selection paths.
use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

mod append;
pub use append::append_existing_nodes;
mod list;
mod node_row;
pub use list::list_nodes;
pub(crate) mod payload;
mod raw_witness;
pub(crate) use node_row::NodeRow;
pub(crate) mod reachability;
mod relations;
mod thread;
pub(crate) use raw_witness::RawWitness;
pub(crate) use thread::{ThreadScope, selection_revision};
mod search_bodies;
pub(crate) mod search_scope;
pub use relations::{provenance, relations};
pub(crate) use search_bodies::search_bodies;

use crate::{
    protocol::{
        ApiError, ErrorCode, HistoryBody, HistoryExecutionLink, HistoryGetArgs, HistoryListArgs,
        HistorySelection, HistorySnippet, MAX_BODY_BYTES, MAX_LIST_LIMIT, Page, PageCursor,
        ThreadDetails, ThreadForkArgs,
    },
    storage::{DbWorker, StorageError},
};

#[derive(Debug, Clone)]
pub struct History {
    database: DbWorker,
}

impl History {
    pub fn new(database: DbWorker) -> Self {
        Self { database }
    }

    pub async fn list(&self, args: HistoryListArgs) -> Result<Page<HistorySnippet>, ApiError> {
        self.database
            .call(move |connection| Ok(list_nodes(connection, &args)))
            .await
            .map_err(storage_error)?
    }

    pub async fn get(&self, args: HistoryGetArgs) -> Result<HistoryBody, ApiError> {
        self.database
            .call(move |connection| Ok(get_node(connection, &args)))
            .await
            .map_err(storage_error)?
    }

    pub async fn relations(
        &self,
        args: crate::protocol::HistoryRelationsArgs,
    ) -> Result<Page<crate::protocol::RelationItem>, ApiError> {
        self.database
            .call(move |connection| Ok(relations(connection, &args)))
            .await
            .map_err(storage_error)?
    }

    /// Persistence-only fork: inherit the current reference. Explicit `agents`
    /// requires Control's thread.fork validation, caller audit and file workers.
    pub async fn fork(&self, args: ThreadForkArgs) -> Result<ThreadDetails, ApiError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        self.database
            .call(move |connection| {
                let tx = connection.transaction()?;
                if let Err(error) = fork_thread(&tx, &args, &id, &now) {
                    return Ok(Err(error));
                }
                let details = tx.query_row(
                    "SELECT cwd,profile_id,model_id,effort,parent_thread_id,agents_path FROM thread WHERE id=?1",
                    [&id],
                    |row| {
                        Ok(ThreadDetails { usage_totals:None, agents_path: row.get(5)?,
                            context_usage: None,
                            context: None,
                            thread_id: id.clone(),
                            title: String::new(),
                            cwd: row.get(0)?,
                            connection: row.get(1)?,
                            model: row.get(2)?,
                            effort: row.get(3)?,
                            parent_id: row.get(4)?,
                            run_id: None,
                            turn_id: None,
                            children_count: 0,
                            shells_count: 0,
                            recovery_reason: None,
                        })
                    },
                )?;
                tx.commit()?;
                Ok(Ok(details))
            })
            .await
            .map_err(storage_error)?
    }
}

/// Append once, inside the same transaction as the operation's durable receipt.
/// The caller must propagate errors and commit only after all related writes.
#[allow(clippy::too_many_arguments)]
pub fn append_node(
    tx: &Transaction<'_>,
    thread_id: &str,
    node_id: &str,
    turn_id: Option<&str>,
    kind: &str,
    payload: &Value,
    now: &str,
) -> Result<(), StorageError> {
    let previous: Option<String> = tx.query_row(
        "SELECT cursor_node_id FROM thread WHERE id=?1 AND deleted_at IS NULL",
        [thread_id],
        |row| row.get(0),
    )?;
    let last: Option<String> = tx
        .query_row(
            "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal DESC LIMIT 1",
            [thread_id],
            |row| row.get(0),
        )
        .optional()?;
    if previous != last {
        return Err(StorageError::Schema(
            "Thread cursor does not match selected path".into(),
        ));
    }
    if let Some(turn_id) = turn_id {
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM turn WHERE id=?1 AND thread_id=?2)",
            params![turn_id, thread_id],
            |row| row.get(0),
        )?;
        if !valid {
            return Err(StorageError::from(ApiError::new(
                ErrorCode::InvalidState,
                "node Turn belongs to another Thread or does not exist",
            )));
        }
    }
    let body = payload.to_string();
    let hash = crate::digest::sha256_hex(body.as_bytes());
    tx.execute(
        "INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![node_id,turn_id,kind,now,body,body.len() as i64,hash],
    )?;
    if let Some(previous) = previous {
        tx.execute(
            "INSERT INTO edge(src,dst) VALUES(?1,?2)",
            params![previous, node_id],
        )?;
    }
    tx.execute(
        "INSERT INTO thread_path(thread_id,ordinal,node_id) SELECT ?1,COALESCE(MAX(ordinal)+1,0),?2 FROM thread_path WHERE thread_id=?1",
        params![thread_id,node_id],
    )?;
    tx.execute(
        "UPDATE thread SET cursor_node_id=?2,selection_revision=selection_revision+1,revision=revision+1,updated_at=?3 WHERE id=?1",
        params![thread_id,node_id,now],
    )?;
    bump_root_revision(tx)
}

/// Adopt an exact sequence with compare-and-swap; node payloads stay unchanged.
/// Replacement/source rows must be saved by the caller in this same transaction.
pub fn select_path(
    tx: &Transaction<'_>,
    thread_id: &str,
    expected_selection_revision: u64,
    path: &[String],
    now: &str,
) -> Result<(), StorageError> {
    let revision: i64 = selection_revision(tx, thread_id, ThreadScope::Live)?;
    if u64::try_from(revision).ok() != Some(expected_selection_revision) {
        return Err(StorageError::from(ApiError::new(
            ErrorCode::InvalidState,
            "selected path revision changed; discard late adoption",
        )));
    }
    let mut seen = HashSet::new();
    for id in path {
        if !seen.insert(id) {
            return Err(StorageError::from(ApiError::new(
                ErrorCode::InvalidState,
                "selected path repeats a node",
            )));
        }
        tx.query_row("SELECT id FROM node WHERE id=?1", [id], |row| {
            row.get::<_, String>(0)
        })?;
    }
    for pair in path.windows(2) {
        append::insert_edge_if_missing(tx, &pair[0], &pair[1])?;
    }
    tx.execute("DELETE FROM thread_path WHERE thread_id=?1", [thread_id])?;
    for (ordinal, id) in path.iter().enumerate() {
        tx.execute(
            "INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES(?1,?2,?3)",
            params![thread_id, ordinal as i64, id],
        )?;
    }
    tx.execute(
        "UPDATE thread SET cursor_node_id=?2,selection_revision=selection_revision+1,revision=revision+1,updated_at=?3 WHERE id=?1 AND selection_revision=?4",
        params![thread_id,path.last(),now,revision],
    )?;
    bump_root_revision(tx)
}

fn bump_root_revision(tx: &Transaction<'_>) -> Result<(), StorageError> {
    let changed = tx.execute(
        "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
        [],
    )?;
    if changed != 1 {
        return Err(StorageError::Schema(
            "root revision metadata missing".into(),
        ));
    }
    Ok(())
}

/// Share the chosen prefix and its sources; never copy immutable node bodies.
/// Calling code can put this and its operation receipt in one transaction.
/// Explicit `agents` is rejected before writes: use Control thread.fork for a
/// validated, audited override; this primitive only inherits the saved reference.
pub fn fork_thread(
    tx: &Transaction<'_>,
    args: &ThreadForkArgs,
    new_thread_id: &str,
    now: &str,
) -> Result<(), ApiError> {
    if args.agents.is_some() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Explicit AGENTS references require Control thread.fork; History forks only inherit the saved reference",
        ));
    }
    let source = tx.query_row(
        "SELECT cwd,agents_path,profile_id,model_id,effort,reasoning_epoch,profile_revision FROM thread WHERE id=?1 AND deleted_at IS NULL",
        [&args.thread_id],
        |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,i64>(5)?,row.get::<_,i64>(6)?)),
    ).optional().map_err(sql_error)?.ok_or_else(|| ApiError::new(ErrorCode::ThreadNotFound,"source Thread not found"))?;
    let ordinal: i64 = tx
        .query_row(
            "SELECT ordinal FROM thread_path WHERE thread_id=?1 AND node_id=?2",
            params![args.thread_id, args.node_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidArgument,
                "fork node is not on the selected path",
            )
        })?;
    validate_group_boundary(tx, &args.thread_id, ordinal)?;
    let (cwd, agents_path) = if let Some(cwd) = &args.cwd {
        let path = std::path::Path::new(cwd);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::path::Path::new(&source.0).join(path)
        };
        let mut normalized = std::path::PathBuf::new();
        for component in absolute.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                value => normalized.push(value.as_os_str()),
            }
        }
        (normalized.to_string_lossy().into_owned(), source.1)
    } else {
        (source.0, source.1)
    };
    tx.execute(
        "INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,effort,reasoning_epoch,profile_revision,cursor_node_id,selection_revision,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,?11)",
        params![new_thread_id,args.thread_id,cwd,agents_path,source.2,source.3,source.4,source.5,source.6,args.node_id,now],
    ).map_err(sql_error)?;
    tx.execute(
        "INSERT INTO thread_path(thread_id,ordinal,node_id) SELECT ?1,ordinal,node_id FROM thread_path WHERE thread_id=?2 AND ordinal<=?3 ORDER BY ordinal",
        params![new_thread_id,args.thread_id,ordinal],
    ).map_err(sql_error)?;
    tx.execute(
        "INSERT INTO thread_replacement(thread_id,replacement_id) SELECT ?1,tr.replacement_id FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN thread_path p ON p.node_id=r.output_node_id AND p.thread_id=?1 WHERE tr.thread_id=?2",
        params![new_thread_id,args.thread_id],
    ).map_err(sql_error)?;
    tx.execute(
        "INSERT INTO attachment_selection(thread_id,attachment_id,state,reason,selected_at,selected_order,revision,protected) SELECT ?1,s.attachment_id,s.state,s.reason,s.selected_at,s.selected_order,s.revision,s.protected FROM attachment_selection s JOIN attachment_ref a ON a.id=s.attachment_id JOIN thread_path p ON p.node_id=a.source_node_id AND p.thread_id=?1 WHERE s.thread_id=?2",
        params![new_thread_id,args.thread_id],
    ).map_err(sql_error)?;
    bump_root_revision(tx).map_err(storage_error)
}

fn validate_group_boundary(
    tx: &Transaction<'_>,
    thread_id: &str,
    ordinal: i64,
) -> Result<(), ApiError> {
    let mut statement = tx.prepare("SELECT n.id,n.payload_json FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal<=?2 ORDER BY p.ordinal").map_err(sql_error)?;
    let mut rows = statement
        .query(params![thread_id, ordinal])
        .map_err(sql_error)?;
    let mut pending: HashSet<String> = HashSet::new();
    let mut group_ids = HashSet::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let body: Option<String> = row.get(1).map_err(sql_error)?;
        let Some(body) = body else { continue };
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidState,
                "stored history payload is malformed",
            )
        })?;
        if let Some(group_id) = payload.get("group_id").and_then(Value::as_str) {
            group_ids.insert(group_id.to_owned());
        }
        let output = payload.get("output").and_then(Value::as_array).or_else(|| {
            payload
                .pointer("/response/output")
                .and_then(Value::as_array)
        });
        let items: Vec<&Value> = output.map_or_else(
            || vec![payload.get("item").unwrap_or(&payload)],
            |items| items.iter().collect(),
        );
        let calls: Vec<&str> = items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .filter_map(|item| item.get("call_id").and_then(Value::as_str))
            .collect();
        if !calls.is_empty() && !pending.is_empty() {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "selected prefix starts another tool group before all results",
            ));
        }
        for call in calls {
            if !pending.insert(call.to_owned()) {
                return Err(ApiError::new(
                    ErrorCode::InvalidState,
                    "duplicate tool call in stored history",
                ));
            }
        }
        for item in items {
            if item.get("type").and_then(Value::as_str) == Some("function_call_output") {
                let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                    return Err(ApiError::new(
                        ErrorCode::InvalidState,
                        "stored tool result has no call_id",
                    ));
                };
                if !pending.remove(call_id) {
                    return Err(ApiError::new(
                        ErrorCode::InvalidState,
                        "selected prefix has an unmatched tool result",
                    ));
                }
            }
        }
    }
    if !pending.is_empty() {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            "fork would split tool calls from their complete results",
        ));
    }
    // Check persisted groups as well, including a group whose response metadata
    // is retained in output_item rather than the provider JSON envelope.
    let mut statement = tx.prepare("SELECT DISTINCT g.id FROM tool_group g JOIN output_item o ON o.attempt_id=g.attempt_id JOIN thread_path p ON p.node_id=o.node_id WHERE p.thread_id=?1 AND p.ordinal<=?2").map_err(sql_error)?;
    let stored_groups = statement
        .query_map(params![thread_id, ordinal], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    group_ids.extend(stored_groups);
    for group_id in group_ids {
        let missing:bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tool_call c LEFT JOIN tool_result r ON r.group_id=c.group_id AND r.call_id=c.call_id LEFT JOIN thread_path p ON p.thread_id=?2 AND p.node_id=r.node_id AND p.ordinal<=?3 WHERE c.group_id=?1 AND p.node_id IS NULL)",
            params![group_id,thread_id,ordinal],|row| row.get(0),
        ).map_err(sql_error)?;
        if missing {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "fork would omit a persisted tool result",
            ));
        }
    }
    Ok(())
}

// A request node can contain many output items and calls. EXISTS preserves one
// history row, and the validated ledger relation never reads IDs from text.
const EXECUTION_RELATION: &str = "FROM (SELECT c.group_id,c.call_id FROM output_item o JOIN tool_group g ON g.attempt_id=o.attempt_id JOIN tool_call c ON c.group_id=g.id WHERE o.node_id=?1 UNION SELECT tr.group_id,tr.call_id FROM tool_result tr WHERE tr.node_id=?1) links JOIN tool_call c ON c.group_id=links.group_id AND c.call_id=links.call_id JOIN tool_group g ON g.id=c.group_id JOIN turn u ON u.id=g.turn_id JOIN shell_execution s ON s.id=c.operation_id AND s.turn_id=g.turn_id AND s.owner_run_id=u.run_id WHERE 1";
fn execution_links(
    connection: &Connection,
    node: &str,
) -> Result<(Vec<HistoryExecutionLink>, u64), ApiError> {
    let count: u64 = connection
        .query_row(
            &format!("SELECT COUNT(*) {EXECUTION_RELATION}"),
            [node],
            |r| r.get::<_, i64>(0).map(|n| n as u64),
        )
        .map_err(sql_error)?;
    let sql = format!(
        "SELECT c.group_id,c.call_id,c.ordinal,c.state,s.id,s.state,s.cwd,substr(COALESCE(json_extract(s.shell_json,'$.command'),''),1,81),(SELECT tr.node_id FROM tool_result tr WHERE tr.group_id=c.group_id AND tr.call_id=c.call_id),(SELECT CASE WHEN COUNT(DISTINCT o.node_id)=1 THEN MIN(o.node_id) END FROM output_item o WHERE o.attempt_id=g.attempt_id) {EXECUTION_RELATION} ORDER BY g.rowid,c.ordinal LIMIT 8"
    );
    let mut statement = connection.prepare(&sql).map_err(sql_error)?;
    let rows = statement
        .query_map([node], |row| {
            let command: String = row.get(7)?;
            let command_truncated = command.chars().count() > 80;
            let command_excerpt = command
                .chars()
                .take(80)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            Ok(HistoryExecutionLink {
                group_id: row.get(0)?,
                call_id: row.get(1)?,
                call_ordinal: row.get::<_, i64>(2)? as u64,
                call_state: row.get(3)?,
                execution_id: row.get(4)?,
                state: row.get(5)?,
                cwd: row.get(6)?,
                command_excerpt,
                command_truncated,
                result_node_id: row.get(8)?,
                request_node_id: row.get(9)?,
            })
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    Ok((rows, count))
}

pub fn get_node(connection: &Connection, args: &HistoryGetArgs) -> Result<HistoryBody, ApiError> {
    if args.limit_bytes == 0 || args.limit_bytes > MAX_BODY_BYTES {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "history body limit must be in 1..=65536",
        ));
    }
    let row = NodeRow::load_payload(connection, &args.node_id)
        .map_err(sql_error)?
        .ok_or_else(|| ApiError::new(ErrorCode::TargetUnavailable, "history node not found"))?;
    let encoding = if args.encoding.eq_ignore_ascii_case("utf-8")
        || args.encoding.eq_ignore_ascii_case("utf8")
    {
        "UTF-8"
    } else {
        args.encoding.as_str()
    };
    let start = if let Some(token) = &args.cursor {
        PageCursor::decode(
            token,
            "history.get",
            &args.node_id,
            row.revision as u64,
            Some(encoding),
        )?
        .last_sort_key
        .parse::<usize>()
        .map_err(|_| ApiError::new(ErrorCode::CursorExpired, "invalid body position"))?
    } else {
        0
    };
    if encoding != "UTF-8" {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "conversation JSON text is UTF-8; use shell.read for raw re-decoding",
        ));
    }
    let text = if let Some(body) = &row.payload {
        if matches!(
            row.kind.as_str(),
            "incomplete_response" | "model_stream_observation"
        ) {
            // Audit originals are exact saved JSON, independent of replay projection.
            body.clone()
        } else {
            let value: Value = serde_json::from_str(body).map_err(|_| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "stored history payload is malformed",
                )
            })?;
            text_of_payload(&value)
        }
    } else {
        String::new()
    };
    if start > text.len() || !text.is_char_boundary(start) {
        return Err(ApiError::new(
            ErrorCode::CursorExpired,
            "body position is unavailable",
        ));
    }
    let end = text.floor_char_boundary(
        start
            .saturating_add(args.limit_bytes as usize)
            .min(text.len()),
    );
    if end == start && end < text.len() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "limit_bytes must fit the next UTF-8 character",
        ));
    }
    let next_cursor = if end < text.len() {
        Some(
            PageCursor {
                version: 1,
                scope: "history.get".into(),
                filter: args.node_id.clone(),
                last_sort_key: end.to_string(),
                watermark: text.len() as u64,
                revision: row.revision as u64,
                encoding: Some(encoding.into()),
            }
            .encode()?,
        )
    } else {
        None
    };
    let sources = reference_ids(
        connection,
        "SELECT s.source_node_id FROM replacement_source s JOIN replacement r ON r.id=s.replacement_id WHERE r.output_node_id=?1 ORDER BY r.created_at,r.id,s.ordinal",
        &args.node_id,
    )?;
    let previous = reference_ids(
        connection,
        "SELECT src FROM edge WHERE dst=?1 ORDER BY src",
        &args.node_id,
    )?;
    let next = reference_ids(
        connection,
        "SELECT dst FROM edge WHERE src=?1 ORDER BY dst",
        &args.node_id,
    )?;
    Ok(HistoryBody {
        context: crate::context::for_node(connection, &args.node_id)?,
        provenance: provenance(connection, &args.node_id)?,
        node_id: args.node_id.clone(),
        kind: row.kind,
        text: text[start..end].into(),
        source_node_ids: sources,
        previous_node_ids: previous,
        next_node_ids: next,
        next_cursor,
        tombstone_reason: if row.payload.is_none() {
            Some(
                row.deleted_reason
                    .unwrap_or_else(|| "body unavailable".into()),
            )
        } else {
            None
        },
    })
}

fn reference_ids(connection: &Connection, sql: &str, id: &str) -> Result<Vec<String>, ApiError> {
    let mut statement = connection.prepare(sql).map_err(sql_error)?;
    let mut rows = statement.query([id]).map_err(sql_error)?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        if ids.len() == 4096 {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "history node has more than 4096 references",
            ));
        }
        ids.push(row.get(0).map_err(sql_error)?);
    }
    Ok(ids)
}

/// Extract only known natural-language fields. Opaque reasoning is never text.
pub fn text_of_payload(payload: &Value) -> String {
    let mut text = String::new();
    let mut seen = false;
    payload::visit(payload, |leaf| {
        if seen {
            text.push('\n');
        }
        seen = true;
        text.push_str(leaf.text);
    });
    text
}

/// Display-only excerpt of a selected conversational node, excluding tools and reasoning.
pub(crate) fn conversation_excerpt(payload: &Value, limit: usize) -> String {
    let mut text = String::new();
    let mut count = 0;
    payload::visit(payload, |leaf| {
        if leaf.context.kind != payload::LeafKind::Natural
            || leaf.context.reasoning
            || leaf
                .context
                .role
                .is_some_and(|role| !matches!(role, "user" | "assistant"))
            || leaf.text.is_empty()
            || count >= limit
        {
            return;
        }
        if count > 0 {
            text.push('\n');
            count += 1;
        }
        for ch in leaf.text.chars().take(limit.saturating_sub(count)) {
            text.push(ch);
            count += 1;
        }
    });
    text
}

fn sql_error(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("history database operation failed: {error}"))
}
fn storage_error(error: StorageError) -> ApiError {
    error.into_api_error()
}
