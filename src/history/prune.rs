//! Explicit release of a Thread's hold on originals of adopted replacements (#235).
//!
//! `thread prune` only marks: it sets `replacement.state='pruned'` and enqueues
//! an ordinary history `gc_job`. The reachability rules in
//! [`super::reachability`] then stop counting this Thread's Turn ownership and
//! the pruned replacement's provenance for the sources, and the existing GC
//! reclaims whatever no other root holds. No table, worker or queue is added.
use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use crate::protocol::{ApiError, ErrorCode};

/// Distinct source nodes (and replacements) one prune may cover.
pub(crate) const CLOSURE_LIMIT: usize = 65_536;
/// Tombstone reason recorded by GC for a node whose only holder was pruned.
pub(crate) const PRUNED_REASON: &str = "pruned_unreachable";

fn sql_error(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("prune database operation failed: {error}"))
}

struct Candidate {
    id: String,
    spill: bool,
    nested: bool,
}

pub fn prune_thread(
    tx: &Transaction<'_>,
    thread: &str,
    before: &str,
    dry_run: bool,
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
    let boundary: i64 = tx
        .query_row(
            "SELECT ordinal FROM thread_path WHERE thread_id=?1 AND node_id=?2",
            params![thread, before],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidArgument,
                "--before must be a node on the Thread's selected path",
            )
        })?;
    let already_pruned: i64 = tx
        .query_row(
            "SELECT count(*) FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN thread_path p ON p.thread_id=tr.thread_id AND p.node_id=r.output_node_id WHERE tr.thread_id=?1 AND r.state='pruned' AND p.ordinal<?2",
            params![thread, boundary],
            |r| r.get(0),
        )
        .map_err(sql_error)?;

    // Level 0: adopted replacements held by the Thread whose output precedes the node.
    let mut level: Vec<(String, String, bool)> = {
        let mut statement = tx
            .prepare("SELECT r.id,r.output_node_id,n.kind='spill' FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN thread_path p ON p.thread_id=tr.thread_id AND p.node_id=r.output_node_id JOIN node n ON n.id=r.output_node_id WHERE tr.thread_id=?1 AND r.state='completed' AND p.ordinal<?2 ORDER BY p.ordinal LIMIT ?3")
            .map_err(sql_error)?;
        statement
            .query_map(params![thread, boundary, CLOSURE_LIMIT as i64 + 1], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?
    };
    let mut visited: HashSet<String> = HashSet::new();
    let mut chosen: Vec<Candidate> = Vec::new();
    let (mut shared_summaries, mut shared_spills) = (0u64, 0u64);
    let mut nested = false;
    // source id -> (payload bytes, kept by a live receipt, on another live path)
    let mut facts: HashMap<String, (u64, bool, bool)> = HashMap::new();
    while !level.is_empty() {
        level.retain(|(id, _, _)| visited.insert(id.clone()));
        if visited.len() > CLOSURE_LIMIT {
            return Err(too_large());
        }
        let held_elsewhere = held_by_other_live_threads(tx, thread, &level)?;
        let mut next: Vec<(String, String, bool)> = Vec::new();
        for (id, _, spill) in level {
            if held_elsewhere.contains(&id) {
                if spill {
                    shared_spills += 1;
                } else {
                    shared_summaries += 1;
                }
                continue;
            }
            // Facts and closure in one pass; the limit trips as sources arrive.
            let rows = {
                let mut statement = tx
                    .prepare_cached("SELECT s.source_node_id,COALESCE(octet_length(n.payload_json),0),EXISTS(SELECT 1 FROM receipt rc JOIN thread rt ON rt.id=rc.target_thread_id WHERE rc.message_node_id=s.source_node_id AND rt.deleted_at IS NULL),EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=s.source_node_id AND p.thread_id<>?2 AND t.deleted_at IS NULL) FROM replacement_source s JOIN node n ON n.id=s.source_node_id WHERE s.replacement_id=?1")
                    .map_err(sql_error)?;
                statement
                    .query_map(params![id, thread], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, bool>(2)?,
                            r.get::<_, bool>(3)?,
                        ))
                    })
                    .map_err(sql_error)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(sql_error)?
            };
            let mut sources = Vec::with_capacity(rows.len());
            for (source, bytes, receipt, elsewhere) in rows {
                if facts.len() >= CLOSURE_LIMIT && !facts.contains_key(&source) {
                    return Err(too_large());
                }
                facts.insert(source.clone(), (bytes.max(0) as u64, receipt, elsewhere));
                sources.push(source);
            }
            for source in sources {
                // A source that is itself an adopted replacement output of this
                // Thread (a nested summary) is part of the same closure.
                let mut statement = tx
                    .prepare_cached("SELECT r.id,r.output_node_id,n.kind='spill' FROM replacement r JOIN thread_replacement tr ON tr.replacement_id=r.id AND tr.thread_id=?2 JOIN node n ON n.id=r.output_node_id WHERE r.output_node_id=?1 AND r.state='completed'")
                    .map_err(sql_error)?;
                let found = statement
                    .query_map(params![source, thread], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .map_err(sql_error)?
                    .collect::<rusqlite::Result<Vec<(String, String, bool)>>>()
                    .map_err(sql_error)?;
                next.extend(found);
            }
            chosen.push(Candidate { id, spill, nested });
        }
        nested = true;
        level = next;
    }

    // A message with a receipt to a live Thread stays a root (rule 3): prune
    // never releases it, so it is reported apart from the candidates.
    let kept_receipt = facts.values().filter(|f| f.1).count();
    let candidates = facts.values().filter(|f| !f.1);
    let payload_bytes: u64 = candidates.clone().map(|f| f.0).sum();
    let source_count = candidates.clone().count();
    let on_other_paths = candidates.filter(|f| f.2).count();
    let summaries = chosen.iter().filter(|c| !c.spill).count();
    let spills = chosen.iter().filter(|c| c.spill).count();
    let nested_count = chosen.iter().filter(|c| c.nested).count();

    let mut gc_job_id = Value::Null;
    if !dry_run && !chosen.is_empty() {
        let ids: Vec<&str> = chosen.iter().map(|c| c.id.as_str()).collect();
        for chunk in ids.chunks(1000) {
            tx.execute(
                "UPDATE replacement SET state='pruned' WHERE state='completed' AND id IN (SELECT value FROM json_each(?1))",
                [serde_json::to_string(chunk).expect("ids serialize")],
            )
            .map_err(sql_error)?;
        }
        tx.execute(
            "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
            [],
        )
        .map_err(sql_error)?;
        let revision: i64 = tx
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'",
                [],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        let job = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO gc_job(id,kind,root_revision,cursor_json,state) VALUES(?1,'history',?2,?3,'pending')",
            params![
                job,
                revision,
                json!({"pruned_thread_id":thread,"replacements":chosen.len()}).to_string()
            ],
        )
        .map_err(sql_error)?;
        gc_job_id = Value::String(job);
    }
    Ok(json!({
        "thread_id": thread,
        "before_node_id": before,
        "dry_run": dry_run,
        "replacements": {"summaries": summaries, "spills": spills, "nested": nested_count},
        "shared_replacements_kept": {"summaries": shared_summaries, "spills": shared_spills},
        "already_pruned_replacements": already_pruned,
        "source_nodes": source_count,
        "kept_receipt_messages": kept_receipt,
        "source_nodes_on_other_live_paths": on_other_paths,
        "candidate_payload_bytes": payload_bytes,
        "reclaimable_bytes": null,
        "reclaimable_reason": "reclamation is decided by the history GC against current references; no estimate is made",
        "lost": ["summary_restore", "original_search", "partial_summary_cache", "rewind_into_range"],
        "gc_job_id": gc_job_id,
    }))
}

fn too_large() -> ApiError {
    ApiError::new(
        ErrorCode::InputLimit,
        format!(
            "prune would cover more than {CLOSURE_LIMIT} nodes or replacements; pass an earlier --before"
        ),
    )
}

/// Replacements in `level` that another live Thread holds: a registration, or
/// the output on that Thread's selected path (a fork made after adoption).
fn held_by_other_live_threads(
    tx: &Transaction<'_>,
    thread: &str,
    level: &[(String, String, bool)],
) -> Result<HashSet<String>, ApiError> {
    let mut held = HashSet::new();
    if level.is_empty() {
        return Ok(held);
    }
    let ids: Vec<&str> = level.iter().map(|(id, _, _)| id.as_str()).collect();
    // thread_replacement has no reverse index: one scan per level, not per row.
    let mut statement = tx
        .prepare("SELECT DISTINCT tr.replacement_id FROM thread_replacement tr JOIN thread t ON t.id=tr.thread_id WHERE tr.thread_id<>?2 AND t.deleted_at IS NULL AND tr.replacement_id IN (SELECT value FROM json_each(?1))")
        .map_err(sql_error)?;
    for id in statement
        .query_map(
            params![serde_json::to_string(&ids).expect("ids serialize"), thread],
            |r| r.get::<_, String>(0),
        )
        .map_err(sql_error)?
    {
        held.insert(id.map_err(sql_error)?);
    }
    for (id, output, _) in level {
        let on_path: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=?1 AND p.thread_id<>?2 AND t.deleted_at IS NULL)",
                params![output, thread],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if on_path {
            held.insert(id.clone());
        }
    }
    Ok(held)
}
