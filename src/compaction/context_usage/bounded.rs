//! Finite display evidence, never the canonical proactive-compaction trigger.
//! Each recent turn contributes its exact newest matching completed attempt
//! through `attempts_completed_scope` (one seek per turn, at most 33 turns).
//! A thread has one open turn at a time, so the newest turn holding a match
//! holds the thread's latest attempt. Only a full window with no match at all
//! is incomplete; a full window that found a witness is not.
use super::*;

pub(crate) fn latest_completed_cte() -> &'static str {
    "recent_turns AS MATERIALIZED (
 SELECT id FROM turn INDEXED BY turns_by_thread WHERE thread_id=?1
 ORDER BY created_at DESC, rowid DESC LIMIT 33
), per_turn AS MATERIALIZED (
 SELECT (SELECT a.rowid FROM model_attempt a INDEXED BY attempts_completed_scope
   WHERE a.turn_id=u.id AND a.state='completed' AND a.profile_id=t.profile_id
   AND a.model_id=t.model_id ORDER BY a.rowid DESC LIMIT 1) AS native_rowid
 FROM recent_turns u CROSS JOIN thread t ON t.id=?1
), selected AS MATERIALIZED (
 SELECT native_rowid FROM per_turn WHERE native_rowid IS NOT NULL
 ORDER BY native_rowid DESC LIMIT 1
)"
}

pub(super) fn query() -> String {
    format!(
        "WITH {}, audit AS MATERIALIZED (
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
 selected.native_rowid IS NULL AND (SELECT count(*) FROM recent_turns)=33,
 (SELECT count(*) FROM audit)=201 OR EXISTS(SELECT 1 FROM edits WHERE octet_length(created_at)>?2),
 selected.native_rowid IS NOT NULL
 FROM (SELECT 1) root LEFT JOIN selected ON 1
 LEFT JOIN model_attempt a ON a.rowid=selected.native_rowid",
        latest_completed_cte(),
        usage::sql_projection("a.usage_json")
    )
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
