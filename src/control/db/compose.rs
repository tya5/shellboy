//! Owner-side durable compose state. Call within the command's receipt transaction.
use super::{error, sql_error};
use crate::ui::{DraftSnapshot, UiAttachment};
use crate::{
    control::submission_retention,
    history::{self, ThreadScope},
    protocol::{
        DraftGetArgs, DraftListArgs, DraftListFilters, DraftListKind, DraftListPage, DraftRecord,
        DraftSaveArgs, ErrorCode, PageCursor, RecordPage,
    },
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;
use std::path::PathBuf;

pub(super) fn save(
    tx: &Transaction<'_>,
    args: &DraftSaveArgs,
    draft_bytes: u64,
    now: &str,
) -> super::Result<()> {
    let DraftSaveArgs {
        thread_id: thread,
        draft,
        anchor,
        follow_tail,
    } = args;
    history::selection_revision(tx, thread, ThreadScope::Live)
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| {
            error(
                ErrorCode::TargetUnavailable,
                "Draft Thread is not available",
            )
        })?;
    if !follow_tail
        && let Some(node) = anchor.node_id.as_deref()
        && tx
            .query_row(
                history::SELECTED_NODE_ORDINAL_SQL,
                params![thread, node],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(sql_error)?
            .is_none()
    {
        return Err(error(
            ErrorCode::CursorExpired,
            "Saved view anchor is no longer selected",
        ));
    }
    crate::ui::draft::validate_positions(draft, draft_bytes)?;
    let revision = i64::try_from(draft.revision)
        .map_err(|_| error(ErrorCode::InvalidArgument, "Draft revision is out of range"))?;
    let offset = i64::try_from(anchor.char_offset)
        .map_err(|_| error(ErrorCode::InvalidArgument, "Anchor offset is out of range"))?;
    let body = serde_json::to_string(draft)
        .map_err(|_| error(ErrorCode::InvalidArgument, "Draft cannot be encoded"))?;
    if body.len() > crate::protocol::MAX_FRAME_BYTES {
        return Err(error(
            ErrorCode::InputLimit,
            "Draft exceeds the input budget",
        ));
    }
    let saved: Option<(Option<String>, i64, String)> = tx
        .query_row(
            "SELECT thread_id,revision,state FROM draft WHERE id=?1",
            [&draft.draft_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sql_error)?;
    if saved.is_some_and(|(owner, old_revision, state)| {
        owner.as_deref() != Some(thread.as_str()) || old_revision > revision || state != "editing"
    }) {
        return Err(error(
            ErrorCode::OperationConflict,
            "Draft revision changed",
        ));
    }
    let fence = submission_retention::fence("?1", "?3", "?2");
    let updated = tx
        .execute(
            &format!(
                "INSERT INTO draft(id,thread_id,revision,body_json,state,updated_at)
                 SELECT ?1,?2,?3,?4,'editing',?5 WHERE NOT {fence}
                 ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,
                    body_json=excluded.body_json,updated_at=excluded.updated_at"
            ),
            params![draft.draft_id, thread, revision, body, now],
        )
        .map_err(sql_error)?;
    if updated == 0 {
        return Err(error(
            ErrorCode::OperationConflict,
            "Draft revision was already submitted",
        )
        .details(serde_json::json!({"reason":"draft_revision_submitted"})));
    }
    tx.execute(
        "INSERT INTO view_state(thread_id,anchor_node_id,anchor_offset,follow_tail)
         VALUES(?1,?2,?3,?4) ON CONFLICT(thread_id) DO UPDATE SET
         anchor_node_id=excluded.anchor_node_id,anchor_offset=excluded.anchor_offset,
         follow_tail=excluded.follow_tail",
        params![thread, anchor.node_id, offset, follow_tail],
    )
    .map_err(sql_error)?;
    Ok(())
}

pub(super) fn get(
    tx: &Transaction<'_>,
    args: &DraftGetArgs,
) -> super::Result<crate::protocol::DraftState> {
    if let Some(operation) = &args.operation_id {
        let record = tx
            .query_row(
                &format!("{EXACT_EVIDENCE}{BINDINGS}{SUBMISSION_GROUP}"),
                params![
                    operation,
                    super::DraftSubmission::id_for_operation(operation),
                    args.thread_id
                ],
                submitted_row,
            )
            .optional()
            .map_err(sql_error)?
            .map(|(_, _, record)| record)
            .ok_or_else(|| error(ErrorCode::TargetUnavailable, "Submitted draft not found"))?;
        let DraftRecord::Submitted { thread_id, .. } = &record else {
            unreachable!()
        };
        let selection_revision = live_selection(tx, thread_id)?;
        return Ok(crate::protocol::DraftState {
            selection_revision,
            draft: None,
            view: None,
            record: Some(record),
        });
    }
    let thread = args
        .thread_id
        .as_deref()
        .ok_or_else(|| error(ErrorCode::InvalidArgument, "Missing draft Thread"))?;
    let selection_revision = live_selection(tx, thread)?;
    let fence = submission_retention::fence("current.id", "current.revision", "?1");
    let body: Option<String> = tx.query_row(
        &format!("SELECT body_json FROM draft current WHERE state='editing' AND thread_id=?1 AND NOT {fence} ORDER BY updated_at DESC,id DESC LIMIT 1"),
        [thread],
        |row| row.get(0),
    ).optional().map_err(sql_error)?;
    let draft = body
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| error(ErrorCode::InvalidState, "Saved draft is malformed"))?;
    let view = tx
        .query_row(
            "SELECT anchor_node_id,anchor_offset,follow_tail FROM view_state WHERE thread_id=?1",
            [thread],
            |row| {
                Ok(crate::protocol::DraftView {
                    anchor: crate::ui::ViewAnchor {
                        node_id: row.get(0)?,
                        char_offset: usize::try_from(row.get::<_, i64>(1)?)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, -1))?,
                    },
                    follow_tail: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(sql_error)?;
    Ok(crate::protocol::DraftState {
        selection_revision,
        draft,
        view,
        record: None,
    })
}

fn live_selection(tx: &Transaction<'_>, thread: &str) -> super::Result<u64> {
    history::selection_revision(tx, thread, ThreadScope::Live)
        .optional()
        .map_err(sql_error)?
        .map(|revision| revision as u64)
        .ok_or_else(|| {
            error(
                ErrorCode::TargetUnavailable,
                "Draft Thread is not available",
            )
        })
}

// Exact reconciliation uses the frozen copy's primary key and the unique
// delivery operation index, even when the caller never learned the Thread.
const EXACT_EVIDENCE: &str = "WITH evidence AS (
    SELECT json_extract(body_json,'$._submission.operation_id') AS operation_id,
           json_extract(body_json,'$._submission.source_draft_id') AS source_draft_id,
           json_extract(body_json,'$._submission.source_revision') AS source_revision,
           json_extract(body_json,'$._submission.message_id') AS message_id,
           updated_at AS accepted_at, thread_id
      FROM draft WHERE id=?2 AND state='submitted' AND (?3 IS NULL OR thread_id=?3)
       AND json_extract(body_json,'$._submission.operation_id')=?1
    UNION ALL
    SELECT client_operation_id,json_extract(result_json,'$.draft_submission.source_draft_id'),
           json_extract(result_json,'$.draft_submission.source_revision'),message_node_id,created_at,target_thread_id
      FROM receipt WHERE client_operation_id=?1 AND (?3 IS NULL OR target_thread_id=?3)
       AND json_extract(result_json,'$.draft_submission.operation_id')=client_operation_id
       AND json_extract(result_json,'$.draft_submission.message_id')=message_node_id
)";
const LIST_EVIDENCE: &str = "WITH evidence AS (
    SELECT json_extract(body_json,'$._submission.operation_id') AS operation_id,
           json_extract(body_json,'$._submission.source_draft_id') AS source_draft_id,
           json_extract(body_json,'$._submission.source_revision') AS source_revision,
           json_extract(body_json,'$._submission.message_id') AS message_id,
           updated_at AS accepted_at, thread_id
      FROM draft WHERE thread_id=?1 AND state='submitted'
    UNION ALL
    SELECT client_operation_id,json_extract(result_json,'$.draft_submission.source_draft_id'),
           json_extract(result_json,'$.draft_submission.source_revision'),message_node_id,created_at,target_thread_id
      FROM receipt WHERE target_thread_id=?1
       AND json_extract(result_json,'$.draft_submission.operation_id')=client_operation_id
       AND json_extract(result_json,'$.draft_submission.message_id')=message_node_id
)";
// Identical copy and receipt evidence describe one record. Conflicting
// bindings for an operation cannot win by timestamp.
const BINDINGS: &str = ", bindings AS (
    SELECT max(accepted_at) AS accepted_at,operation_id,source_draft_id,source_revision,message_id,thread_id
      FROM evidence GROUP BY operation_id,source_draft_id,source_revision,message_id,thread_id
)
SELECT max(accepted_at) AS accepted_at,operation_id,source_draft_id,source_revision,message_id,
       count(*) AS bindings,thread_id FROM bindings";
const SUBMISSION_GROUP: &str = " GROUP BY operation_id";

type DraftRow = (String, String, DraftRecord);
fn submitted_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DraftRow> {
    if row.get::<_, i64>(5)? != 1 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let operation_id: String = row.get(1)?;
    let source_draft_id: String = row.get(2)?;
    let revision: i64 = row.get(3)?;
    let source_revision = u64::try_from(revision)
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, revision))?;
    let message_id: String = row.get(4)?;
    if operation_id.is_empty() || source_draft_id.is_empty() || message_id.is_empty() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok((
        row.get(0)?,
        operation_id.clone(),
        DraftRecord::Submitted {
            draft_id: super::DraftSubmission::id_for_operation(&operation_id),
            thread_id: row.get(6)?,
            operation_id,
            source_draft_id,
            source_revision,
            message_id,
        },
    ))
}

pub(super) fn list(tx: &Transaction<'_>, args: &DraftListArgs) -> super::Result<DraftListPage> {
    let filters = DraftListFilters {
        thread_id: args.thread_id.clone(),
        kind: args.kind,
    };
    if let (DraftListKind::Submitted, Some(thread)) = (args.kind, &args.thread_id) {
        history::selection_revision(tx, thread, ThreadScope::Live)
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| {
                error(
                    ErrorCode::TargetUnavailable,
                    "Draft Thread is not available",
                )
            })?;
    }
    let scope = "draft.list";
    let filter = serde_json::to_string(&filters).expect("draft list filter is serializable");
    let boundary: Option<(String, String)> = args
        .cursor
        .as_deref()
        .map(|token| {
            let cursor = PageCursor::decode_live(token, scope, &filter, 0, None)?;
            serde_json::from_str(&cursor.last_sort_key)
                .map_err(|_| error(ErrorCode::CursorExpired, "Invalid draft list cursor"))
        })
        .transpose()?;
    let (time, key) = boundary.unzip();
    let count = i64::from(args.limit) + 1;
    let mut rows = (|| -> rusqlite::Result<Vec<DraftRow>> {
        match args.kind {
            DraftListKind::Restorable => {
                let fence = submission_retention::fence(
                    "json_extract(current.body_json,'$._submission.source_draft_id')",
                    "json_extract(current.body_json,'$._submission.source_revision')",
                    "COALESCE(current.thread_id,?1)",
                );
                tx.prepare(&format!("SELECT updated_at,id,json_extract(body_json,'$.kind'),json_extract(body_json,'$.source_message_id'),substr(json_extract(body_json,'$.text'),1,80) FROM draft current WHERE state IN ('archived','submitting') AND (thread_id IS NULL OR (thread_id=?1 AND EXISTS(SELECT 1 FROM thread t WHERE t.id=current.thread_id AND t.deleted_at IS NULL))) AND (state<>'submitting' OR NOT {fence}) AND (?2 IS NULL OR (updated_at,id)<(?2,?3)) ORDER BY updated_at DESC,id DESC LIMIT ?4"))?
                    .query_map(params![args.thread_id,time,key,count], |row| {
                        let id: String = row.get(1)?;
                        let record = if row.get::<_, Option<String>>(2)?.as_deref() == Some("cancelled_message") {
                            DraftRecord::Cancelled { draft_id: id.clone(), source_message_id: row.get(3)? }
                        } else {
                            DraftRecord::Unconfirmed { draft_id: id.clone(), excerpt: row.get::<_, Option<String>>(4)?.unwrap_or_default() }
                        };
                        Ok((row.get(0)?,id,record))
                    })?.collect()
            }
            DraftListKind::Submitted => {
                tx.prepare(&format!("{LIST_EVIDENCE}{BINDINGS}{SUBMISSION_GROUP} HAVING ?2 IS NULL OR (max(accepted_at),operation_id)<(?2,?3) ORDER BY accepted_at DESC,operation_id DESC LIMIT ?4"))?
                    .query_map(params![args.thread_id,time,key,count], submitted_row)?.collect()
            }
        }
    })().map_err(sql_error)?;
    let next_cursor = if rows.len() > args.limit as usize {
        rows.pop();
        let (time, id, _) = rows.last().expect("validated positive page limit");
        Some(
            PageCursor {
                version: 2,
                scope: scope.into(),
                filter,
                last_sort_key: serde_json::to_string(&(time, id))
                    .expect("draft list key is serializable"),
                watermark: 0,
                revision: 0,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    Ok(RecordPage::new(
        args.limit,
        filters,
        rows.into_iter().map(|(_, _, record)| record).collect(),
        next_cursor,
    ))
}

pub(super) fn restore_archive(
    tx: &Transaction<'_>,
    id: &str,
    draft_bytes: u64,
) -> super::Result<crate::ui::DraftSnapshot> {
    let submission_fence = crate::control::submission_retention::fence(
        "json_extract(current.body_json,'$._submission.source_draft_id')",
        "json_extract(current.body_json,'$._submission.source_revision')",
        "current.thread_id",
    );
    let value=tx.query_row(&format!("SELECT body_json FROM draft current WHERE id=?1 AND state IN ('archived','submitting') AND (current.thread_id IS NULL OR EXISTS(SELECT 1 FROM thread t WHERE t.id=current.thread_id AND t.deleted_at IS NULL)) AND (state<>'submitting' OR NOT {submission_fence})"),[id],|r|r.get::<_,String>(0)).optional().map_err(sql_error)?.ok_or_else(||error(ErrorCode::TargetUnavailable,"Restorable draft not found"))?;
    let value: Value = serde_json::from_str(&value)
        .map_err(|_| error(ErrorCode::InvalidState, "Archived draft is malformed"))?;
    let snapshot = if value["kind"] == "cancelled_message" {
        let source = value["source_message_id"]
            .as_str()
            .ok_or_else(|| error(ErrorCode::InvalidState, "Archived source is missing"))?;
        let (text, attachments) = restore_source(tx, source, draft_bytes)?;
        let operation = value["source_operation_id"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let frozen=tx.query_row("SELECT body_json FROM draft WHERE state='submitted' AND json_extract(body_json,'$._submission.operation_id')=?1 LIMIT 1",[operation],|row|row.get::<_,String>(0)).optional().map_err(sql_error)?;
        let mut snapshot = frozen
            .and_then(|body| serde_json::from_str::<DraftSnapshot>(&body).ok())
            .filter(|snapshot| snapshot.text == text)
            .unwrap_or_default();
        snapshot.text = text;
        snapshot.cursor = snapshot.text.len();
        snapshot.attachments = attachments;
        snapshot.edit = None;
        snapshot.literal_paste = true;
        snapshot
    } else {
        serde_json::from_value::<DraftSnapshot>(value)
            .map_err(|_| error(ErrorCode::InvalidState, "Frozen draft is malformed"))?
    };
    crate::ui::draft::validate_positions(&snapshot, draft_bytes)?;
    crate::ui::draft::decode(&snapshot.text, snapshot.attachments.clone())?;
    Ok(snapshot)
}

fn restore_source(
    tx: &Transaction<'_>,
    node_id: &str,
    limit: u64,
) -> super::Result<(String, Vec<UiAttachment>)> {
    let body: Option<Option<String>> = tx
        .query_row(
            "SELECT payload_json FROM node WHERE id=?1",
            [&node_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    let Some(Some(body)) = body else {
        return Err(error(
            ErrorCode::OutputExpired,
            "Saved request source expired; its pending snapshot cannot be reconstructed",
        ));
    };
    let payload: Value = serde_json::from_str(&body)
        .map_err(|_| error(ErrorCode::InvalidState, "Saved request is malformed"))?;
    let attachments=tx.prepare("SELECT id,absolute_path,input_kind FROM attachment_ref WHERE source_node_id=?1 ORDER BY id").map_err(sql_error)?.query_map([node_id],|r|Ok(UiAttachment{id:r.get(0)?,path:PathBuf::from(r.get::<_,String>(1)?),kind:r.get(2)?,state:"saved source; revalidated on send".into(),position:None})).map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    let (text, attachments) = crate::ui::draft::restore(&payload, attachments, false)?;
    if text.len() as u64 > limit {
        return Err(error(
            ErrorCode::InputLimit,
            "Saved request exceeds current draft limit; no text was truncated",
        ));
    }
    Ok((text, attachments))
}
