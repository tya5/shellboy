//! Finite display evidence, never the canonical proactive-compaction trigger.
use super::*;

pub(super) fn query() -> String {
    format!("WITH recent_turns AS MATERIALIZED (
 SELECT id FROM turn INDEXED BY turns_by_thread WHERE thread_id=?1
 ORDER BY created_at DESC LIMIT 33
), per_turn AS MATERIALIZED (
 SELECT (SELECT json_group_array(native_rowid) FROM
   (SELECT rowid AS native_rowid FROM model_attempt INDEXED BY attempts_by_turn
    WHERE turn_id=u.id ORDER BY started_at DESC LIMIT 33)) AS rowids
 FROM recent_turns u
), selected AS MATERIALIZED (
 SELECT a.rowid AS native_rowid FROM per_turn u CROSS JOIN json_each(u.rowids) c
 CROSS JOIN model_attempt a ON a.rowid=c.value CROSS JOIN thread t ON t.id=?1
 WHERE a.state='completed' AND a.profile_id=t.profile_id AND a.model_id=t.model_id
 ORDER BY a.rowid DESC LIMIT 1
), audit AS MATERIALIZED (
 SELECT replacement_id FROM thread_replacement WHERE thread_id=?1
 ORDER BY replacement_id DESC LIMIT 201
), edits AS MATERIALIZED (
 SELECT r.created_at FROM audit tr CROSS JOIN replacement r ON r.id=tr.replacement_id
)
 SELECT CASE WHEN octet_length(a.id)<=?2 THEN a.id END, {},
 CASE WHEN octet_length(a.started_at)<=?2 THEN a.started_at END,
 CASE WHEN octet_length(a.ended_at)<=?2 THEN a.ended_at END,
 EXISTS(SELECT 1 FROM edits WHERE octet_length(created_at)<=?2 AND created_at>a.started_at),
 COALESCE(octet_length(a.id)>?2 OR octet_length(a.started_at)>?2
 OR COALESCE(octet_length(a.ended_at)>?2,0),0),
 (SELECT count(*) FROM recent_turns)=33 OR EXISTS(SELECT 1 FROM per_turn WHERE json_array_length(rowids)=33),
 (SELECT count(*) FROM audit)=201 OR EXISTS(SELECT 1 FROM edits WHERE octet_length(created_at)>?2),
 selected.native_rowid IS NOT NULL
 FROM (SELECT 1) root LEFT JOIN selected ON 1
 LEFT JOIN model_attempt a ON a.rowid=selected.native_rowid", usage::sql_projection("a.usage_json"))
}

pub(super) fn read(
    db: &Connection,
    thread: &str,
    limit: i64,
    scope: Scope,
) -> Result<RawContextUsage, StorageError> {
    db.query_row(&query(), params![thread, limit], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            StandardUsage::read(row, 1)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, bool>(8)?,
            row.get::<_, bool>(9)?,
            row.get::<_, bool>(10)?,
            row.get::<_, bool>(11)?,
            row.get::<_, bool>(12)?,
        ))
    })
    .map_err(StorageError::from)
    .and_then(
        |(id, usage, started, ended, pending, invalid, partial, audit_partial, present)| {
            if invalid {
                return Err(scalar_limit());
            }
            let latest = if present {
                Some(RawSuccess {
                    attempt_id: id.ok_or_else(scalar_limit)?,
                    usage,
                    started_at: started.ok_or_else(scalar_limit)?,
                    ended_at: ended,
                    next_request_pending: pending,
                })
            } else {
                None
            };
            Ok(RawContextUsage {
                scope,
                latest,
                coverage: Some(Coverage {
                    latest_complete: !partial,
                    pending_complete: !audit_partial,
                }),
            })
        },
    )
}
