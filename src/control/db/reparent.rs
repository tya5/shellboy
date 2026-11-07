//! Move a Thread under another live Thread, or detach it (#222 stage 2).
//!
//! Only `thread.parent_thread_id` changes. Live authority follows `run.owner_run_id`
//! (stop, retry, recovery and completion notices, wait cycles), which is stored when
//! a Run starts and never re-derived, so a move leaves every Run, shell, wait and
//! pending reply untouched. The new parent is read at exactly one point: the next
//! model send that starts a Run on the moved Thread. `updated_at` is not changed so
//! list order does not move; `revision` is bumped like every Thread write.
use super::{MutationResult, actor, error, sql_error};
use crate::protocol::{ErrorCode, Origin};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;

/// Most ancestors one move walks. Real trees are shallow; a chain longer than this
/// (or a pre-existing cycle above the new parent) is refused instead of walked on.
pub(crate) const MAX_ANCESTOR_STEPS: usize = 1024;

/// What a successful call changed. `None` from [`apply`] means the parent was already
/// the requested one.
pub(super) struct Moved {
    pub previous: Option<String>,
    pub current: Option<String>,
}

/// `requested` is the wire value: empty detaches. Returns `None` when nothing changes.
pub(super) fn apply(
    tx: &Transaction<'_>,
    thread: &str,
    requested: &str,
    caller: Option<&Origin>,
    now: &str,
) -> super::Result<Option<Moved>> {
    let new_parent = (!requested.is_empty()).then_some(requested);
    let previous: Option<String> = tx
        .query_row(
            "SELECT parent_thread_id FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [thread],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| error(ErrorCode::ThreadNotFound, "Thread not found"))?;
    if previous.as_deref() == new_parent {
        return Ok(None);
    }
    if let Some(parent) = new_parent {
        if parent == thread {
            return Err(error(
                ErrorCode::InvalidState,
                "a Thread cannot be its own parent",
            ));
        }
        let live: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                [parent],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if !live {
            return Err(error(ErrorCode::ThreadNotFound, "parent Thread not found"));
        }
        // The forest is acyclic because every write passes through here (or create,
        // which only points at existing Threads), so the walk is a simple chain.
        let mut cursor = parent.to_owned();
        for _ in 0..=MAX_ANCESTOR_STEPS {
            if cursor == thread {
                return Err(error(
                    ErrorCode::InvalidState,
                    "the new parent is a descendant of this Thread; move would make the Thread its own ancestor",
                ));
            }
            let next: Option<String> = tx
                .query_row(
                    "SELECT parent_thread_id FROM thread WHERE id=?1",
                    [&cursor],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_error)?
                .flatten();
            let Some(next) = next else {
                return finish(tx, thread, previous, new_parent, caller, now);
            };
            cursor = next;
        }
        return Err(error(
            ErrorCode::InputLimit,
            format!("the new parent has more than {MAX_ANCESTOR_STEPS} ancestors"),
        ));
    }
    finish(tx, thread, previous, None, caller, now)
}

fn finish(
    tx: &Transaction<'_>,
    thread: &str,
    previous: Option<String>,
    current: Option<&str>,
    caller: Option<&Origin>,
    now: &str,
) -> super::Result<Option<Moved>> {
    tx.execute(
        "UPDATE thread SET parent_thread_id=?2,revision=revision+1 WHERE id=?1 AND deleted_at IS NULL",
        params![thread, current],
    )
    .map_err(sql_error)?;
    super::super::context::record(
        tx,
        thread,
        "thread_parent",
        actor(caller),
        json!({"before":{"parent_id":previous},"after":{"parent_id":current}}),
        now,
    )
    .map_err(sql_error)?;
    Ok(Some(Moved {
        previous,
        current: current.map(str::to_owned),
    }))
}

/// Threads whose view changes: the moved one first, then the old and the new parent.
pub(super) fn add_changed(result: &mut MutationResult, thread: &str, moved: &Moved) {
    for id in std::iter::once(thread)
        .chain(moved.previous.as_deref())
        .chain(moved.current.as_deref())
    {
        if !result.changed_threads.iter().any(|known| known == id) {
            result.changed_threads.push(id.to_owned());
        }
    }
}

/// Tell the caller which Run, if any, still answers to its previous owner.
pub(super) fn annotate(
    tx: &Transaction<'_>,
    result: &mut MutationResult,
    thread: &str,
    moved: &Moved,
) -> super::Result<()> {
    let live: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT id,owner_run_id FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",
            [thread],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    result.result["parent_change"] = json!({
        "previous_parent_id": moved.previous,
        "live_run_id": live.as_ref().map(|(run, _)| run),
        "live_run_owner_run_id": live.and_then(|(_, owner)| owner),
    });
    Ok(())
}
