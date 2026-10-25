//! Successful request attachment consumption and hash baseline adoption.
use super::*;

/// Establish comparison baselines atomically with the driver's completed state.
pub fn mark_success_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedAttachments,
    attempt_id: &str,
) -> Result<(), ApiError> {
    let ids: Vec<_> = prepared
        .manifest
        .iter()
        .map(|m| m.attachment_id.clone())
        .collect();
    mark_success_rows_tx(tx, &prepared.thread_id, attempt_id, &ids)
}
pub(super) fn mark_success_rows_tx(
    tx: &Transaction<'_>,
    thread: &str,
    attempt: &str,
    ids: &[String],
) -> Result<(), ApiError> {
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE a.id=?1 AND a.state='completed' AND a.generation=u.generation AND u.thread_id=?2)",params![attempt,thread],|row|row.get(0)).map_err(sql_error)?;
    if !valid {
        return Err(stale());
    }
    for id in ids {
        tx.execute("UPDATE attachment_send SET state='sent' WHERE attempt_id=?1 AND attachment_id=?2 AND state='ready'",params![attempt,id]).map_err(sql_error)?;
        tx.execute(
            "UPDATE attachment_selection SET protected=0 WHERE thread_id=?1 AND attachment_id=?2 AND EXISTS(SELECT 1 FROM attachment_send s WHERE s.attempt_id=?3 AND s.attachment_id=?2 AND s.state IN ('ready','sent','unavailable','excluded'))",
            params![thread, id, attempt],
        )
        .map_err(sql_error)?;
    }
    Ok(())
}
