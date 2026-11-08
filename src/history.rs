//! Immutable conversation nodes and exact, versioned Thread selection paths.
use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

mod append;
pub use append::append_existing_nodes;
mod list;
mod window;
pub use window::{STATEMENTS_FOR_TESTS as WINDOW_STATEMENTS_FOR_TESTS, window};
mod node_row;
pub(crate) mod observation;
pub use list::list_nodes;
pub(crate) mod payload;
mod raw_witness;
pub(crate) use node_row::NodeRow;
pub mod branches;
pub mod prune;
pub(crate) mod reachability;
pub mod rewind;
/// Integration-test hook for read visibility.
#[doc(hidden)]
pub fn reachability_for_tests(db: &Connection, id: &str) -> rusqlite::Result<bool> {
    reachability::searchable(db, id)
}
mod relations;
mod thread;
pub(crate) use raw_witness::{RawWitness, RetainedExecutionFacts, RetainedExecutionRequest};
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
                        Ok(ThreadDetails { instruction_sources: None, archived_at: None, tags: Vec::new(), auto_compact_at: None, effective_auto_compact_at: None, segment_chars: None, effective_segment_chars: None, reduce_rounds: None, effective_reduce_rounds: None, awaiting_reply: Vec::new(), awaiting_thread_replies: Vec::new(), awaiting_human: Vec::new(), awaiting_replies: Vec::new(), last_end_message: None, last_input: None,  last_compaction:None, usage_totals:None, agents_path: row.get(5)?,
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
                            parent_change: None,
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
    let previous = selected_ids(tx, thread_id)?;
    if let Some(edit_point) = edit_invalidates_kept_reasoning(tx, &previous, path)? {
        restart_reasoning_after(tx, thread_id, &path[..edit_point])?;
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

fn selected_ids(db: &Connection, thread_id: &str) -> Result<Vec<String>, StorageError> {
    Ok(db
        .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
        .query_map([thread_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether a node carries assistant output (a response or its spill copy),
/// which may hold reasoning or signed thinking bound to its prefix, and the
/// reasoning epoch it was saved with.
fn output_epoch(db: &Connection, id: &str) -> Result<Option<Option<i64>>, StorageError> {
    Ok(db
        .query_row(
            "SELECT kind<>'tool_result' AND json_type(payload_json,'$.output')='array',json_extract(payload_json,'$.reasoning_epoch') FROM node WHERE id=?1",
            [id],
            |row| Ok((row.get::<_, Option<bool>>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()?
        .and_then(|(output, epoch)| output.unwrap_or(false).then_some(epoch)))
}
fn carries_output(db: &Connection, id: &str) -> Result<bool, StorageError> {
    Ok(output_epoch(db, id)?.is_some())
}

/// #182: an edit at the first changed position P keeps assistant output saved
/// against the old prefix when any node after P carries output; returns P.
/// The node at P itself (a new spill or restored original) keeps its own
/// unchanged prefix. Appends and truncations change nothing before a kept
/// response.
fn edit_invalidates_kept_reasoning(
    db: &Connection,
    previous: &[String],
    path: &[String],
) -> Result<Option<usize>, StorageError> {
    let unchanged = previous
        .iter()
        .zip(path)
        .take_while(|(old, new)| old == new)
        .count();
    if unchanged == previous.len() {
        return Ok(None);
    }
    for id in path.iter().skip(unchanged + 1) {
        if carries_output(db, id)? {
            return Ok(Some(unchanged));
        }
    }
    Ok(None)
}

/// Per-Thread set of output nodes whose reasoning stays sendable although it
/// was saved under an older epoch (#182). Valid only for the epoch it names;
/// a model change or binding retry advances the epoch and voids it.
pub(crate) fn reasoning_keep_key(thread_id: &str) -> String {
    format!("reasoning.keep.{thread_id}")
}
pub(crate) fn reasoning_keep(
    db: &Connection,
    thread_id: &str,
    epoch: i64,
) -> Result<HashSet<String>, StorageError> {
    let saved: Option<String> = db
        .query_row(
            "SELECT value FROM app_meta WHERE key=?1",
            [reasoning_keep_key(thread_id)],
            |row| row.get(0),
        )
        .optional()?;
    let Some(value) = saved.and_then(|body| serde_json::from_str::<Value>(&body).ok()) else {
        return Ok(HashSet::new());
    };
    if value["epoch"].as_i64() != Some(epoch) {
        return Ok(HashSet::new());
    }
    Ok(value["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

/// #182 positional drop: reasoning strictly before the earliest edited
/// position stays byte-identical (whatever was sendable there remains so);
/// reasoning saved at or after it is never sent again. Implemented as a new
/// epoch plus the kept set of the unchanged prefix.
fn restart_reasoning_after(
    tx: &Transaction<'_>,
    thread_id: &str,
    unchanged: &[String],
) -> Result<(), StorageError> {
    let epoch: i64 = tx.query_row(
        "SELECT reasoning_epoch FROM thread WHERE id=?1",
        [thread_id],
        |row| row.get(0),
    )?;
    let previous = reasoning_keep(tx, thread_id, epoch)?;
    let mut kept = Vec::new();
    for id in unchanged {
        if let Some(saved) = output_epoch(tx, id)?
            && (saved == Some(epoch) || previous.contains(id))
        {
            kept.push(id.clone());
        }
    }
    tx.execute(
        "UPDATE thread SET reasoning_epoch=reasoning_epoch+1 WHERE id=?1",
        [thread_id],
    )?;
    tx.execute(
        "INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![
            reasoning_keep_key(thread_id),
            serde_json::json!({"epoch": epoch + 1, "nodes": kept}).to_string()
        ],
    )?;
    Ok(())
}

/// Bound for [`summary_message_ids`]: the restore provenance budget.
const SUMMARY_SOURCE_ROWS: i64 = 65_536;
/// #182: the user-message IDs (a message ID is its message node ID) covered by
/// any summary node (ordinary, active-Turn or nested), in original order,
/// derived on demand from the ordered replacement sources. Nested summaries
/// are expanded through their own sources, at most 8 levels and 65,536 source
/// rows; nothing extra is stored on summary nodes.
pub fn summary_message_ids(
    db: &Connection,
    summary_node_id: &str,
) -> Result<Vec<String>, StorageError> {
    let mut statement = db.prepare(
        "WITH RECURSIVE src(node,depth,sort) AS(
           SELECT s.source_node_id,1,printf('%08d',s.ordinal) FROM replacement r JOIN replacement_source s ON s.replacement_id=r.id WHERE r.output_node_id=?1
           UNION ALL
           SELECT s.source_node_id,src.depth+1,src.sort||'.'||printf('%08d',s.ordinal) FROM src JOIN replacement r ON r.output_node_id=src.node JOIN replacement_source s ON s.replacement_id=r.id WHERE src.depth<8
           LIMIT ?2)
         SELECT n.id FROM src JOIN node n ON n.id=src.node
         WHERE n.kind NOT IN ('tool_result','assistant_response')
           AND (json_extract(n.payload_json,'$.role')='user' OR json_extract(n.payload_json,'$.item.role')='user')
         ORDER BY src.sort",
    )?;
    let rows = statement
        .query_map(params![summary_node_id, SUMMARY_SOURCE_ROWS], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(rows)
}

/// #182: true while the latest assistant output on the selected path requested
/// a tool, i.e. the provider tool loop is still open. Selective automatic edits
/// wait until this is false; only the active-Turn summary may run meanwhile.
pub(crate) fn open_tool_loop(db: &Connection, thread_id: &str) -> Result<bool, StorageError> {
    for id in selected_ids(db, thread_id)?.iter().rev() {
        if carries_output(db, id)? {
            return Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM node n, json_each(n.payload_json,'$.output') o WHERE n.id=?1 AND json_extract(o.value,'$.type')='function_call')",
                [id],
                |row| row.get(0),
            )?);
        }
    }
    Ok(false)
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
        "SELECT cwd,agents_path,profile_id,model_id,effort,reasoning_epoch,profile_revision,auto_compact_at,segment_chars,reduce_rounds FROM thread WHERE id=?1 AND deleted_at IS NULL",
        [&args.thread_id],
        |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,i64>(5)?,row.get::<_,i64>(6)?,row.get::<_,Option<i64>>(7)?,row.get::<_,Option<i64>>(8)?,row.get::<_,Option<i64>>(9)?)),
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
        "INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,effort,reasoning_epoch,profile_revision,cursor_node_id,selection_revision,updated_at,auto_compact_at,segment_chars,reduce_rounds) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,?11,?12,?13,?14)",
        params![new_thread_id,args.thread_id,cwd,agents_path,source.2,source.3,source.4,source.5,source.6,args.node_id,now,source.7,source.8,source.9],
    ).map_err(sql_error)?;
    tx.execute(
        "INSERT INTO thread_path(thread_id,ordinal,node_id) SELECT ?1,ordinal,node_id FROM thread_path WHERE thread_id=?2 AND ordinal<=?3 ORDER BY ordinal",
        params![new_thread_id,args.thread_id,ordinal],
    ).map_err(sql_error)?;
    // #182: the fork shares the parent's epoch, so it shares its kept set.
    tx.execute(
        "INSERT INTO app_meta(key,value) SELECT ?1,value FROM app_meta WHERE key=?2",
        params![
            reasoning_keep_key(new_thread_id),
            reasoning_keep_key(&args.thread_id)
        ],
    )
    .map_err(sql_error)?;
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
#[doc(hidden)]
pub fn execution_links_statement_for_tests() -> String {
    format!(
        "SELECT c.group_id,c.call_id,c.ordinal,c.state,s.id,s.state,s.cwd,substr(COALESCE(json_extract(s.shell_json,'$.command'),''),1,81),(SELECT tr.node_id FROM tool_result tr WHERE tr.group_id=c.group_id AND tr.call_id=c.call_id),(SELECT CASE WHEN COUNT(DISTINCT o.node_id)=1 THEN MIN(o.node_id) END FROM output_item o WHERE o.attempt_id=g.attempt_id),CASE WHEN json_valid(s.shell_json) THEN CASE WHEN json_type(s.shell_json,'$.description')='text' THEN substr(json_extract(s.shell_json,'$.description'),1,80) END END {EXECUTION_RELATION} ORDER BY g.rowid,c.ordinal LIMIT 8"
    )
}
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
    let sql = execution_links_statement_for_tests();
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
                description: crate::tool_description::label(
                    row.get::<_, Option<String>>(10)?.as_deref(),
                ),
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
    read_node(connection, args).map(|(body, _)| body)
}

/// `get_node`, plus the per-part facts when `args.parts` selects a summary's parts.
pub fn read_node(
    connection: &Connection,
    args: &HistoryGetArgs,
) -> Result<(HistoryBody, Option<Value>), ApiError> {
    if args.limit_bytes == 0 || args.limit_bytes > MAX_BODY_BYTES {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "history body limit must be in 1..=65536",
        ));
    }
    let row = NodeRow::load_payload(connection, &args.node_id)
        .map_err(sql_error)?
        .ok_or_else(|| ApiError::new(ErrorCode::TargetUnavailable, "history node not found"))?;
    // Released by `thread prune` and not held elsewhere: not retained, even
    // before the history GC has removed the body (#235).
    if row.deleted_reason.as_deref() == Some(prune::PRUNED_REASON)
        || (row.payload.is_some()
            && reachability::released_by_prune(connection, &args.node_id).map_err(sql_error)?)
    {
        return Err(ApiError::new(
            ErrorCode::TargetUnavailable,
            "node was pruned (thread prune) and is not retained",
        )
        .hint("its summary or spill placeholder is still readable; the original was released on purpose"));
    }
    let encoding = if args.encoding.eq_ignore_ascii_case("utf-8")
        || args.encoding.eq_ignore_ascii_case("utf8")
    {
        "UTF-8"
    } else {
        args.encoding.as_str()
    };
    let cursor_scope = if args.parts {
        if row.kind != "summary" {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Parts require a summary node",
            ));
        }
        "history.get.parts"
    } else if args.end_message_only {
        if row.kind != "assistant_response" {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "End-message projection requires an assistant response node",
            ));
        }
        "history.get.end_message"
    } else {
        "history.get"
    };
    let (start, cursor_watermark) = if let Some(token) = &args.cursor {
        let cursor = PageCursor::decode(
            token,
            cursor_scope,
            &args.node_id,
            row.revision as u64,
            Some(encoding),
        )?;
        let start = cursor
            .last_sort_key
            .parse::<usize>()
            .map_err(|_| ApiError::new(ErrorCode::CursorExpired, "invalid body position"))?;
        (start, Some(cursor.watermark))
    } else {
        (0, None)
    };
    if encoding != "UTF-8" {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "conversation JSON text is UTF-8; use shell.read for raw re-decoding",
        ));
    }
    let mut parts_info = None;
    let text = if args.parts {
        let value: Value = row
            .payload
            .as_deref()
            .and_then(|body| serde_json::from_str(body).ok())
            .ok_or_else(|| {
                ApiError::new(ErrorCode::TargetUnavailable, "summary body is unavailable")
            })?;
        let (info, text) = crate::compaction::inspect_parts(connection, &value)?;
        parts_info = Some(info);
        // Part records are mutable cache rows: a collected or overwritten
        // part changes the rebuilt text, so a stale cursor must not misalign.
        if cursor_watermark.is_some_and(|mark| mark != text.len() as u64) {
            return Err(ApiError::new(
                ErrorCode::CursorExpired,
                "summary parts changed since the cursor was issued",
            )
            .hint("restart reading without the cursor"));
        }
        text
    } else if let Some(body) = &row.payload {
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
            if args.end_message_only {
                if value.get("output").and_then(Value::as_array).is_some() {
                    end_message_text(&value).unwrap_or_default()
                } else {
                    return Err(ApiError::new(
                        ErrorCode::TargetUnavailable,
                        "No final assistant message is saved in this node",
                    ));
                }
            } else {
                text_of_payload(&value)
            }
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
                scope: cursor_scope.into(),
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
    let body = HistoryBody {
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
    };
    Ok((body, parts_info))
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

/// A read projection only: never treats reasoning/commentary as an end message.
fn end_message_text(payload: &Value) -> Option<String> {
    let output = payload.get("output")?.as_array()?;
    let messages = || {
        output.iter().filter(|item| {
            item["type"] == "message" && matches!(item["role"].as_str(), None | Some("assistant"))
        })
    };
    let message = messages()
        .rev()
        .find(|item| item["phase"] == "final_answer")
        .or_else(|| {
            messages()
                .rev()
                .find(|item| item.get("phase").is_none() || item["phase"].is_null())
        })?;
    let content = message.get("content")?.as_array()?;
    let mut text = Vec::new();
    for part in content {
        let leaf = match part["type"].as_str() {
            Some("output_text" | "text") => part["text"].as_str(),
            Some("refusal") => part["refusal"].as_str(),
            _ => None,
        };
        if let Some(leaf) = leaf {
            text.push(leaf);
        }
    }
    Some(text.join("\n"))
}

/// Extract known natural-language fields; opaque reasoning is never text.
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
