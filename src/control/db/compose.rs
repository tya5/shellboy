//! Owner-side durable compose state. Call within the command's receipt transaction.
use super::{error, sql_error};
use crate::ui::{DraftSnapshot, UiAttachment};
use crate::{
    control::submission_retention,
    history::{self, ThreadScope},
    protocol::{DraftSaveArgs, ErrorCode},
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
    thread: &str,
) -> super::Result<crate::protocol::DraftState> {
    let selection_revision = history::selection_revision(tx, thread, ThreadScope::Live)
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| {
            error(
                ErrorCode::TargetUnavailable,
                "Draft Thread is not available",
            )
        })?;
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
        selection_revision: selection_revision as u64,
        draft,
        view,
    })
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
