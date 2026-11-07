//! `thread rewind` (#236): cut a Thread's selected path back to a node and keep
//! the cut-off part as an ordinary child Thread.
//!
//! Branches are Threads. The tail is preserved by the existing [`fork_thread`]
//! taken at the old end of the path (so undo is opening that child), the path
//! is rewritten by the existing [`select_path`], and the tool-group boundary is
//! checked by the existing `validate_group_boundary`. No table, queue or worker
//! is added. The whole path is rewritten (O(path length)); the measured cost is
//! recorded in `docs/development/decisions.md`.
use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use super::{
    ThreadScope, fork_thread, select_path, selected_ids, selection_revision, sql_error,
    storage_error, validate_group_boundary,
};
use crate::protocol::{ApiError, ErrorCode, ThreadForkArgs};

/// Longest selected path one rewind rewrites (the whole path is copied twice:
/// once into the child, once by `select_path`). Cost: see decisions.md.
pub const PATH_LIMIT: usize = 200_000;
/// Items listed per fact group; counts are always exact.
const LIST_LIMIT: i64 = 20;
/// Nested summaries followed when the node lies inside a summarized range.
const EXPANSION_DEPTH: usize = 64;
/// Originals one summary may expand to (the compaction provenance bound).
const EXPANSION_LIMIT: usize = 65_536;

fn invalid_state(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidState, message)
}

/// Rewind `thread` so its selected path ends at `node`; the previous full path
/// is kept as the new child Thread `branch_id`. With `exclude_node` the path
/// ends at the node just before `node` (rewind to before a chosen request).
///
/// With `dry_run` the very same operation runs inside a savepoint that is
/// rolled back, so the facts and every refusal are exactly what a real rewind
/// would report, and nothing is written (#246). The result then has
/// `"dry_run": true` and names no branch Thread (none exists).
pub fn rewind_thread(
    tx: &Transaction<'_>,
    thread: &str,
    node: &str,
    exclude_node: bool,
    dry_run: bool,
    branch_id: &str,
    now: &str,
) -> Result<Value, ApiError> {
    if !dry_run {
        return rewind_inner(tx, thread, node, exclude_node, branch_id, now);
    }
    tx.execute_batch("SAVEPOINT rewind_dry_run")
        .map_err(sql_error)?;
    let outcome = rewind_inner(tx, thread, node, exclude_node, branch_id, now);
    tx.execute_batch("ROLLBACK TO rewind_dry_run; RELEASE rewind_dry_run")
        .map_err(sql_error)?;
    let mut value = outcome?;
    if let Some(object) = value.as_object_mut() {
        object.remove("branch_thread_id");
        object.insert("dry_run".into(), json!(true));
        object.insert(
            "undo".into(),
            json!(
                "not executed; a real rewind keeps the removed part as a branch Thread you can open"
            ),
        );
    }
    Ok(value)
}

fn rewind_inner(
    tx: &Transaction<'_>,
    thread: &str,
    node: &str,
    exclude_node: bool,
    branch_id: &str,
    now: &str,
) -> Result<Value, ApiError> {
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
            [thread],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if !exists {
        return Err(ApiError::new(ErrorCode::ThreadNotFound, "Thread not found"));
    }
    let live: Option<String> = tx
        .query_row(
            "SELECT id FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",
            [thread],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    if let Some(run) = live {
        return Err(invalid_state(
            "Thread has a live Run; stop it (run stop) or wait for it to end before rewinding",
        )
        .hint("a Run cannot rewind the Thread it is running in: use `shellboy thread fork --thread THREAD --node NODE` to continue from an earlier point in a new Thread, or ask the user to run /rewind")
        .details(json!({"run_id": run})));
    }
    let queued: i64 = tx
        .query_row(
            "SELECT count(*) FROM receipt WHERE target_thread_id=?1 AND state IN ('accepted','held') AND path_admission='unknown'",
            [thread],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if queued > 0 {
        return Err(invalid_state(
            "Thread has messages with unknown path admission; resolve them before rewinding",
        )
        .details(json!({"queued_messages": queued})));
    }

    let length: i64 = tx
        .query_row(
            "SELECT count(*) FROM thread_path WHERE thread_id=?1",
            [thread],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if length as usize > PATH_LIMIT {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "selected path is too long to rewind in one operation",
        )
        .hint("compact the Thread first, or continue from an earlier point with `shellboy thread fork --thread THREAD --node NODE`")
        .details(json!({"path_nodes": length, "limit": PATH_LIMIT})));
    }
    let old_path = selected_ids(tx, thread).map_err(storage_error)?;
    let Some(old_end) = old_path.last().cloned() else {
        return Err(invalid_state("Thread has no saved history to rewind"));
    };
    let revision = selection_revision(tx, thread, ThreadScope::Live).map_err(sql_error)?;

    let (mut new_path, expanded) = target_path(tx, thread, node, &old_path)?;
    if exclude_node {
        new_path.pop();
        if new_path.is_empty() {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "nothing comes before this node; there is no earlier point to rewind to",
            ));
        }
    }
    if new_path.len() == old_path.len() && new_path == old_path {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "node is already the end of the selected path; nothing to rewind",
        ));
    }

    // Facts about what the conversation rewind does not undo (read before any write).
    let boundary_time: String = tx
        .query_row("SELECT created_at FROM node WHERE id=?1", [node], |r| {
            r.get(0)
        })
        .map_err(sql_error)?;
    let shells = shell_facts(tx, thread, &boundary_time)?;

    // 1. Keep the whole old path as a child Thread (existing fork machinery;
    //    refuses to split unfinished tool calls at the old end).
    fork_thread(
        tx,
        &ThreadForkArgs {
            agents: None,
            thread_id: thread.to_owned(),
            node_id: old_end.clone(),
            cwd: None,
        },
        branch_id,
        now,
    )?;
    let children = child_facts(tx, thread, branch_id, &boundary_time)?;
    let replies = pending_replies_sent(tx, thread)?;

    // 2. Attachments whose source message leaves the path are not sent.
    let common = old_path
        .iter()
        .zip(&new_path)
        .take_while(|(a, b)| a == b)
        .count() as i64;
    // The range that leaves the path (read before the rewrite): nodes, and the
    // Turns that own them.
    let removed_turns: i64 = tx
        .query_row(
            "SELECT count(DISTINCT n.turn_id) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal>=?2",
            params![thread, common],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    let removed_attachments = tx
        .execute(
            "DELETE FROM attachment_selection WHERE thread_id=?1 AND attachment_id IN (SELECT a.id FROM attachment_ref a JOIN thread_path p ON p.node_id=a.source_node_id AND p.thread_id=?1 WHERE p.ordinal>=?2)",
            params![thread, common],
        )
        .map_err(sql_error)?;

    // 3. Whole-path rewrite, then the boundary check on the new end. An error
    //    rolls back the child and every other write of this operation.
    select_path(tx, thread, revision as u64, &new_path, now).map_err(storage_error)?;
    validate_group_boundary(tx, thread, new_path.len() as i64 - 1)?;

    Ok(json!({
        "thread_id": thread,
        "node_id": new_path.last(),
        "previous_end_node_id": old_end,
        "branch_thread_id": branch_id,
        "undo": format!("open Thread {branch_id}; it holds the removed part"),
        "path_nodes": {"before": old_path.len(), "after": new_path.len()},
        "removed": {"nodes": old_path.len() as i64 - common, "turns": removed_turns},
        "rewound_into_summarized_range": expanded,
        "attachment_selections_removed": removed_attachments,
        "shell_executions_after_node": shells,
        "child_threads_after_node": children,
        "pending_replies_to_sent_messages": replies,
        "note": "Conversation only: shell side effects are not undone. Background shells started before the rewind may still deliver shell_ended to this Thread, and replies to messages it sent may still arrive.",
    }))
}

/// The new selected path: a prefix of the current path, or (when the node lies
/// inside a summarized range whose originals are still held) the path with that
/// summary expanded back to its originals up to the node.
fn target_path(
    tx: &Transaction<'_>,
    thread: &str,
    node: &str,
    old_path: &[String],
) -> Result<(Vec<String>, bool), ApiError> {
    if let Some(index) = old_path.iter().position(|id| id == node) {
        return Ok((old_path[..=index].to_vec(), false));
    }
    let known: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM node WHERE id=?1)",
            [node],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if !known {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "node is not on the Thread's selected path",
        ));
    }
    let positions: HashMap<&str, usize> = old_path
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    // Follow the node up through the summaries this Thread holds until one is on
    // the path, trying every candidate (innermost first in `chain`).
    let mut saw_pruned = false;
    let Some(chain) = find_chain(tx, thread, node, &positions, 0, &mut saw_pruned)? else {
        return Err(if saw_pruned {
            ApiError::new(
                ErrorCode::TargetUnavailable,
                "This range's originals were pruned (thread prune) and are not retained; rewind cannot go back into it",
            )
        } else {
            ApiError::new(
                ErrorCode::InvalidArgument,
                "node is neither on the Thread's selected path nor inside a summary or spill this Thread holds",
            )
        });
    };
    let mut path = old_path.to_vec();
    let mut expanded_from = usize::MAX;
    for (replacement, output) in chain.iter().rev() {
        let index = path
            .iter()
            .position(|id| id == output)
            .ok_or_else(|| invalid_state("summary changed while rewinding; retry"))?;
        expanded_from = expanded_from.min(index);
        let originals = original_order(tx, replacement, output)?;
        path.splice(index..=index, originals);
        if path.len() > old_path.len() + EXPANSION_LIMIT {
            return Err(invalid_state(
                "summarized range is too large to rewind into",
            ));
        }
    }
    let end = path
        .iter()
        .skip(expanded_from)
        .position(|id| id == node)
        .map(|offset| expanded_from + offset)
        .ok_or_else(|| invalid_state("node is not among the summary's originals"))?;
    path.truncate(end + 1);
    // Every restored original must still have its body.
    let mut seen = HashSet::new();
    let mut statement = tx
        .prepare_cached(
            "SELECT payload_json IS NULL OR deleted_reason IS NOT NULL FROM node WHERE id=?1",
        )
        .map_err(sql_error)?;
    for id in path.iter().skip(expanded_from) {
        if !seen.insert(id) {
            return Err(invalid_state("summary originals repeat a node"));
        }
        let gone: Option<bool> = statement
            .query_row([id], |r| r.get(0))
            .optional()
            .map_err(sql_error)?;
        if gone != Some(false) {
            return Err(ApiError::new(
                ErrorCode::TargetUnavailable,
                "An original in this range is no longer retained; rewind cannot go back into it",
            ));
        }
    }
    Ok((path, true))
}

/// Original order of a summary: the marker's own ordering when it carries one
/// (context markers keep retained anchors), otherwise the saved source order.
fn original_order(
    tx: &Transaction<'_>,
    replacement: &str,
    output: &str,
) -> Result<Vec<String>, ApiError> {
    let payload: Option<String> = tx
        .query_row("SELECT payload_json FROM node WHERE id=?1", [output], |r| {
            r.get(0)
        })
        .map_err(sql_error)?;
    if let Some(order) = payload
        .and_then(|body| serde_json::from_str::<Value>(&body).ok())
        .and_then(|value| {
            value
                .pointer("/context_trim/original_order")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .map(|id| id.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
        })
    {
        return order.ok_or_else(|| invalid_state("summary has invalid source ordering"));
    }
    let ids = tx
        .prepare_cached("SELECT source_node_id FROM replacement_source WHERE replacement_id=?1 ORDER BY ordinal LIMIT ?2")
        .map_err(sql_error)?
        .query_map(params![replacement, EXPANSION_LIMIT as i64 + 1], |r| {
            r.get::<_, String>(0)
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    if ids.is_empty() || ids.len() > EXPANSION_LIMIT {
        return Err(invalid_state(
            "summary source list is unavailable or too large",
        ));
    }
    Ok(ids)
}

/// Shell executions this Thread's Runs started after the node (not undone).
fn shell_facts(tx: &Transaction<'_>, thread: &str, after: &str) -> Result<Value, ApiError> {
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE r.thread_id=?1 AND s.started_at>?2",
            params![thread, after],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    let items = tx
        .prepare("SELECT s.id,s.state,s.cwd,substr(COALESCE(json_extract(s.shell_json,'$.command'),''),1,81),s.started_at FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE r.thread_id=?1 AND s.started_at>?2 ORDER BY s.started_at,s.id LIMIT ?3")
        .map_err(sql_error)?
        .query_map(params![thread, after, LIST_LIMIT], |r| {
            let command: String = r.get(3)?;
            let truncated = command.chars().count() > 80;
            let excerpt: String = command
                .chars()
                .take(80)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            Ok(json!({
                "execution_id": r.get::<_, String>(0)?,
                "state": r.get::<_, String>(1)?,
                "cwd": r.get::<_, String>(2)?,
                "command_excerpt": excerpt,
                "command_truncated": truncated,
                "started_at": r.get::<_, String>(4)?,
            }))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    Ok(json!({"count": count, "items": items, "truncated": count > LIST_LIMIT}))
}

/// Child Threads whose first own activity (or fork node) is after the node.
fn child_facts(
    tx: &Transaction<'_>,
    thread: &str,
    branch_id: &str,
    after: &str,
) -> Result<Value, ApiError> {
    const STARTED: &str = "SELECT c.id,c.title,COALESCE((SELECT min(n.created_at) FROM node n JOIN turn tu ON tu.id=n.turn_id WHERE tu.thread_id=c.id),(SELECT created_at FROM node WHERE id=c.cursor_node_id)) AS started FROM thread c WHERE c.parent_thread_id=?1 AND c.deleted_at IS NULL AND c.id<>?2";
    let count: i64 = tx
        .query_row(
            &format!("SELECT count(*) FROM ({STARTED}) WHERE started>?3"),
            params![thread, branch_id, after],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    let items = tx
        .prepare(&format!(
            "SELECT id,title,started FROM ({STARTED}) WHERE started>?3 ORDER BY started,id LIMIT ?4"
        ))
        .map_err(sql_error)?
        .query_map(params![thread, branch_id, after, LIST_LIMIT], |r| {
            Ok(json!({
                "thread_id": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "started_at": r.get::<_, String>(2)?,
            }))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    Ok(json!({"count": count, "items": items, "truncated": count > LIST_LIMIT}))
}

type Chain = Vec<(String, String)>;

/// Depth-first over the held summaries that list `current` as a source; the
/// result is the chain (replacement, output) from the innermost summary to one
/// whose output is on the path. A pruned candidate is remembered, not fatal.
fn find_chain(
    tx: &Transaction<'_>,
    thread: &str,
    current: &str,
    positions: &HashMap<&str, usize>,
    depth: usize,
    saw_pruned: &mut bool,
) -> Result<Option<Chain>, ApiError> {
    if depth >= EXPANSION_DEPTH {
        return Err(invalid_state("summary nesting is too deep to rewind into"));
    }
    let rows = {
        let mut statement = tx
            .prepare_cached("SELECT r.id,r.output_node_id,r.state FROM replacement r JOIN thread_replacement tr ON tr.replacement_id=r.id AND tr.thread_id=?1 JOIN replacement_source s ON s.replacement_id=r.id WHERE s.source_node_id=?2 AND r.state IN ('completed','pruned') ORDER BY r.created_at DESC,r.id LIMIT 16")
            .map_err(sql_error)?;
        statement
            .query_map(params![thread, current], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?
    };
    for (replacement, output, state) in rows {
        if state == "pruned" {
            *saw_pruned = true;
            continue;
        }
        if positions.contains_key(output.as_str()) {
            return Ok(Some(vec![(replacement, output)]));
        }
        if let Some(mut chain) = find_chain(tx, thread, &output, positions, depth + 1, saw_pruned)?
        {
            chain.insert(0, (replacement, output));
            return Ok(Some(chain));
        }
    }
    Ok(None)
}

/// Reply-expected messages this Thread sent that are still unanswered: their
/// replies can land in the Thread after the rewind.
fn pending_replies_sent(tx: &Transaction<'_>, thread: &str) -> Result<Value, ApiError> {
    const WHERE: &str = "FROM receipt WHERE json_extract(result_json,'$.reply.state')='pending' AND json_extract(sender_json,'$.thread_id')=?1";
    let count: i64 = tx
        .query_row(&format!("SELECT count(*) {WHERE}"), [thread], |r| r.get(0))
        .map_err(sql_error)?;
    let items = tx
        .prepare(&format!(
            "SELECT message_node_id,target_thread_id,created_at {WHERE} ORDER BY created_at,rowid LIMIT ?2"
        ))
        .map_err(sql_error)?
        .query_map(params![thread, LIST_LIMIT], |r| {
            Ok(json!({
                "message_id": r.get::<_, String>(0)?,
                "target_thread_id": r.get::<_, String>(1)?,
                "created_at": r.get::<_, String>(2)?,
            }))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    Ok(json!({"count": count, "items": items, "truncated": count > LIST_LIMIT}))
}
