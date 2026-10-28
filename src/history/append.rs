//! Adopt existing immutable nodes at the selected tail without rewriting it.
use super::*;

/// Append unselected nodes in caller order, with the same single selection/root
/// revision change as `select_path`. Existing selected nodes are skipped. The
/// caller must roll back the transaction on any error.
pub fn append_existing_nodes(
    tx: &Transaction<'_>,
    thread_id: &str,
    expected_selection_revision: u64,
    nodes: &[String],
    now: &str,
) -> Result<usize, StorageError> {
    let revision = selection_revision(tx, thread_id, ThreadScope::Live)?;
    if u64::try_from(revision).ok() != Some(expected_selection_revision) {
        return Err(StorageError::from(ApiError::new(
            ErrorCode::InvalidState,
            "selected path revision changed; discard late adoption",
        )));
    }
    let cursor: Option<String> = tx.query_row(
        "SELECT cursor_node_id FROM thread WHERE id=?1",
        [thread_id],
        |r| r.get(0),
    )?;
    let tail: Option<(i64, String)> = tx.query_row(
        "SELECT ordinal,node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal DESC LIMIT 1",
        [thread_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    if cursor.as_ref() != tail.as_ref().map(|(_, id)| id) {
        return Err(StorageError::Schema(
            "Thread cursor does not match selected path".into(),
        ));
    }
    let mut ordinal = tail.as_ref().map_or(-1, |(ordinal, _)| *ordinal);
    let mut previous = cursor;
    let mut appended = 0;
    for node in nodes {
        let selected: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND node_id=?2)",
            params![thread_id, node],
            |r| r.get(0),
        )?;
        if selected {
            continue;
        }
        tx.query_row("SELECT id FROM node WHERE id=?1", [node], |r| {
            r.get::<_, String>(0)
        })?;
        if let Some(previous) = &previous {
            insert_edge_if_missing(tx, previous, node)?;
        }
        ordinal = ordinal
            .checked_add(1)
            .ok_or_else(|| StorageError::Schema("selected path ordinal overflow".into()))?;
        tx.execute(
            "INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES(?1,?2,?3)",
            params![thread_id, ordinal, node],
        )?;
        previous = Some(node.clone());
        appended += 1;
    }
    if appended != 0 {
        let changed = tx.execute(
            "UPDATE thread SET cursor_node_id=?2,selection_revision=selection_revision+1,revision=revision+1,updated_at=?3 WHERE id=?1 AND selection_revision=?4",
            params![thread_id, previous, now, revision],
        )?;
        if changed != 1 {
            return Err(StorageError::from(ApiError::new(
                ErrorCode::InvalidState,
                "selected path revision changed; discard late adoption",
            )));
        }
        bump_root_revision(tx)?;
    }
    Ok(appended)
}

pub(super) fn insert_edge_if_missing(
    tx: &Transaction<'_>,
    source: &str,
    destination: &str,
) -> Result<(), StorageError> {
    // OR IGNORE alone still invokes BEFORE INSERT cycle traversal for duplicate
    // edges. Filter them before INSERT; new edges retain the existing trigger.
    tx.execute(
        "INSERT INTO edge(src,dst) SELECT ?1,?2 WHERE NOT EXISTS(SELECT 1 FROM edge WHERE src=?1 AND dst=?2)",
        params![source, destination],
    )?;
    Ok(())
}
