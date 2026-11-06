//! Archive and unarchive Threads (#222 stage 1): metadata only, never deletion.
//!
//! Only `thread.archived_at` changes. History, nodes, GC roots, `updated_at` and
//! running work are untouched, so list order does not move. A new Run clears the
//! mark in `assign_message`; nothing archives automatically.
use super::{MutationResult, error, sql_error, target};
use crate::protocol::{ErrorCode, Origin, ThreadArchiveArgs};
use rusqlite::{Transaction, params};
use serde_json::json;
use std::collections::HashSet;

/// Most Threads one call may select with `--descendants` (explicit IDs included).
pub(crate) const MAX_SELECTED: usize = 256;

pub(super) fn run(
    tx: &Transaction<'_>,
    args: &ThreadArchiveArgs,
    caller: Option<&Origin>,
    archive: bool,
    now: &str,
) -> super::Result<MutationResult> {
    let requested: Vec<String> = if args.thread_ids.is_empty() {
        vec![target(None, caller)?]
    } else {
        let mut seen = HashSet::new();
        args.thread_ids
            .iter()
            .filter(|id| seen.insert(id.as_str()))
            .cloned()
            .collect()
    };
    let encoded = serde_json::to_string(&requested).expect("Thread IDs serialize");
    // Bounded in SQL: one more than the cap proves the selection is too large.
    let sql = if args.descendants {
        "WITH RECURSIVE sel(id) AS (
           SELECT id FROM thread WHERE id IN (SELECT value FROM json_each(?1)) AND deleted_at IS NULL
           UNION
           SELECT c.id FROM thread c JOIN sel ON c.parent_thread_id=sel.id WHERE c.deleted_at IS NULL
           LIMIT ?2)
         SELECT t.id,t.archived_at IS NOT NULL FROM sel JOIN thread t ON t.id=sel.id"
    } else {
        // At most 200 validated IDs: no cap needed.
        "SELECT id,archived_at IS NOT NULL FROM thread
         WHERE id IN (SELECT value FROM json_each(?1)) AND deleted_at IS NULL"
    };
    let cap = MAX_SELECTED as i64 + 1;
    let binds: &[&dyn rusqlite::ToSql] = if args.descendants {
        &[&encoded, &cap]
    } else {
        &[&encoded]
    };
    let selected: Vec<(String, bool)> = tx
        .prepare(sql)
        .and_then(|mut statement| {
            statement
                .query_map(binds, |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
                })?
                .collect::<rusqlite::Result<_>>()
        })
        .map_err(sql_error)?;
    if selected.len() > MAX_SELECTED {
        return Err(error(
            ErrorCode::InputLimit,
            format!(
                "selection exceeds {MAX_SELECTED} Threads; archive smaller subtrees separately"
            ),
        ));
    }
    let found: HashSet<&str> = selected.iter().map(|(id, _)| id.as_str()).collect();
    let not_found: Vec<&String> = requested
        .iter()
        .filter(|id| !found.contains(id.as_str()))
        .collect();
    if selected.is_empty() {
        return Err(error(ErrorCode::ThreadNotFound, "Thread not found"));
    }
    // Requested order first, then discovered descendants by ID: deterministic output.
    let mut ordered: Vec<&(String, bool)> = selected.iter().collect();
    ordered.sort_by_key(|(id, _)| {
        (
            requested.iter().position(|r| r == id).unwrap_or(usize::MAX),
            id.clone(),
        )
    });
    let (mut changed, mut already) = (Vec::new(), Vec::new());
    for (id, is_archived) in ordered {
        if *is_archived == archive {
            already.push(id.clone());
            continue;
        }
        tx.execute(
            "UPDATE thread SET archived_at=?2,revision=revision+1 WHERE id=?1 AND deleted_at IS NULL",
            params![id, archive.then_some(now)],
        )
        .map_err(sql_error)?;
        changed.push(id.clone());
    }
    let mut result = MutationResult::value(json!({
        if archive { "archived" } else { "unarchived" }: changed,
        "already": already,
        "not_found": not_found,
        "descendants": args.descendants,
    }))?;
    result.changed_threads = changed;
    Ok(result)
}

/// A new Run is the one event that makes an archived Thread visible again.
pub(super) fn clear_for_new_run(
    tx: &Transaction<'_>,
    thread: &str,
    actor: serde_json::Value,
    now: &str,
) -> rusqlite::Result<()> {
    if tx.execute(
        "UPDATE thread SET archived_at=NULL,revision=revision+1 WHERE id=?1 AND archived_at IS NOT NULL",
        [thread],
    )? > 0
    {
        super::super::context::record(
            tx,
            thread,
            "thread_unarchived",
            actor,
            json!({"reason":"run_started"}),
            now,
        )?;
    }
    Ok(())
}
