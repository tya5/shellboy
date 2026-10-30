//! Bounded queued-message cancellation and foreground delivery facts.
use crate::{
    config::Config,
    protocol::{
        MAX_FRAME_BYTES, MessageCancelArgs, MessageCancelItem, MessageCancelOutcome,
        MessageCancelResult, Origin,
    },
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// A late nudge for already-admitted input must not release a later shell call.
pub(crate) fn has_immediate_pending_tx(db: &Connection, turn: &str) -> rusqlite::Result<bool> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM receipt r WHERE r.turn_id=?1 AND r.state='accepted' AND r.path_admission='unadmitted' AND json_extract(r.result_json,'$.now')=1 AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=r.target_thread_id AND p.node_id=r.message_node_id))",[turn],|r|r.get(0))
}

pub(crate) fn archive_id(message: &str) -> String {
    format!("cancelled-message:{message}")
}

pub(crate) fn cancel_tx(
    tx: &Transaction<'_>,
    args: &MessageCancelArgs,
    caller: Option<&Origin>,
    at: &str,
) -> Result<(MessageCancelResult, Vec<String>), StorageError> {
    use MessageCancelOutcome::*;
    let actor = caller
        .map(|origin| json!({"kind":"model","identity":origin.identity()}))
        .unwrap_or_else(|| json!({"kind":"human"}));
    let mut items = Vec::with_capacity(args.message_ids.len());
    let mut changed = Vec::new();
    for message in &args.message_ids {
        let row = tx.query_row("SELECT r.id,r.target_thread_id,r.sender_json,r.state,r.path_admission,r.client_operation_id,EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=r.target_thread_id AND p.node_id=r.message_node_id),EXISTS(SELECT 1 FROM thread t WHERE t.id=r.target_thread_id AND t.deleted_at IS NULL),EXISTS(SELECT 1 FROM node n WHERE n.id=r.message_node_id AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL),r.created_at FROM receipt r WHERE r.message_node_id=?1",[message],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,bool>(6)?,r.get::<_,bool>(7)?,r.get::<_,bool>(8)?,r.get::<_,String>(9)?))).optional()?;
        let mut item = MessageCancelItem {
            message_id: message.clone(),
            outcome: NotFound,
            thread_id: None,
            archive_draft_id: None,
        };
        let Some((
            receipt,
            thread,
            sender,
            state,
            admission,
            operation,
            selected,
            live,
            body,
            created,
        )) = row
        else {
            items.push(item);
            continue;
        };
        let sender: Value = serde_json::from_str(&sender)
            .map_err(|_| StorageError::Schema("Saved message sender is malformed".into()))?;
        if caller.is_some_and(|origin| {
            sender.get("thread_id").and_then(Value::as_str) != Some(origin.thread_id.as_str())
        }) {
            item.outcome = Forbidden;
            items.push(item);
            continue;
        }
        item.thread_id = Some(thread.clone());
        let archive = archive_id(message);
        if state == "cancelled" {
            item.outcome = AlreadyCancelled;
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM draft WHERE id=?1 AND state='archived' AND json_extract(body_json,'$.kind')='cancelled_message' AND json_extract(body_json,'$.source_message_id')=?2)",params![archive,message],|r|r.get(0))?;
            item.archive_draft_id = exists.then_some(archive);
            items.push(item);
            continue;
        }
        if admission == "admitted" || selected || state == "applied" {
            item.outcome = AlreadyAdmitted;
            items.push(item);
            continue;
        }
        if !live || !body || !matches!(state.as_str(), "accepted" | "held") {
            item.outcome = Unavailable;
            items.push(item);
            continue;
        }
        let archived = json!({"kind":"cancelled_message","source_message_id":message,"source_receipt_id":receipt,"source_operation_id":operation,"source_created_at":created,"cancelled_by":actor,"cancelled_at":at});
        let archived = archived.to_string();
        let collision: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM draft WHERE id=?1)",
            [&archive],
            |r| r.get(0),
        )?;
        if collision || archived.len() > MAX_FRAME_BYTES {
            item.outcome = Unavailable;
            items.push(item);
            continue;
        }
        tx.execute("INSERT INTO draft(id,thread_id,revision,body_json,state,updated_at)VALUES(?1,?2,0,?3,'archived',?4)",params![archive,thread,archived,at])?;
        tx.execute("UPDATE receipt SET state='cancelled',hold_reason=?2,updated_at=?3,result_json=json_set(COALESCE(result_json,'{}'),'$.cancellation',json(?4)) WHERE id=?1",params![receipt,if caller.is_none(){"cancelled_by_human"}else{"cancelled_by_sender"},at,json!({"actor":actor,"at":at,"archive_draft_id":archive}).to_string()])?;
        item.outcome = Cancelled;
        item.archive_draft_id = Some(archive);
        if !changed.contains(&thread) {
            changed.push(thread);
        }
        items.push(item);
    }
    if !changed.is_empty()
        && tx.execute(
            "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
            [],
        )? != 1
    {
        return Err(StorageError::Schema(
            "root revision metadata missing".into(),
        ));
    }
    Ok((MessageCancelResult { items }, changed))
}

fn invalid(message: &str) -> StorageError {
    crate::protocol::ApiError::new(crate::protocol::ErrorCode::InvalidArgument, message).into()
}
fn frozen_serialized_budget(config: &Config) -> u64 {
    config
        .limits
        .draft_bytes
        .saturating_mul(6)
        .saturating_add(MAX_FRAME_BYTES as u64)
}
fn frozen_body(
    operation: &str,
    thread: Option<&str>,
    submission: &super::db::DraftSubmission,
    config: &Config,
) -> Result<Value, StorageError> {
    if operation.len() > 4096
        || submission.source_draft_id.is_empty()
        || submission.source_draft_id.len() > 4096
        || submission.revision > i64::MAX as u64
        || submission.draft_id != super::db::DraftSubmission::id_for_operation(operation)
        || submission.draft_id == submission.source_draft_id
    {
        return Err(invalid("Frozen draft identity/revision is invalid"));
    }
    let mut body = submission.snapshot.clone();
    let Some(object) = body.as_object_mut() else {
        return Err(invalid("Frozen draft must be a snapshot object"));
    };
    if object.get("draft_id").and_then(Value::as_str) != Some(submission.source_draft_id.as_str())
        || object.get("revision").and_then(Value::as_u64) != Some(submission.revision)
        || !object.get("text").is_some_and(Value::is_string)
        || !object.get("attachments").is_some_and(Value::is_array)
    {
        return Err(invalid(
            "Frozen draft snapshot identity/content does not match its submission",
        ));
    }
    if object["text"].as_str().expect("validated text").len() as u64 > config.limits.draft_bytes {
        return Err(crate::protocol::ApiError::new(
            crate::protocol::ErrorCode::InputLimit,
            "Frozen draft text exceeds the draft byte limit; no text was truncated",
        )
        .into());
    }
    object.insert("_submission".into(),json!({"operation_id":operation,"source_draft_id":submission.source_draft_id,"source_revision":submission.revision,"target_thread_id":thread}));
    if body.to_string().len() as u64 > frozen_serialized_budget(config) {
        return Err(crate::protocol::ApiError::new(crate::protocol::ErrorCode::InputLimit,"Frozen draft serialized metadata exceeds its bounded storage allowance; no content was truncated").into());
    }
    Ok(body)
}
pub(super) fn prepare_submission(
    db: &mut Connection,
    operation: &str,
    thread: Option<&str>,
    submission: &super::db::DraftSubmission,
    config: &Config,
) -> Result<(), StorageError> {
    let body = frozen_body(operation, thread, submission, config)?;
    let tx = db.transaction()?;
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT body_json,state FROM draft WHERE id=?1",
            [&submission.draft_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((saved, state)) = existing {
        let mut saved: Value = serde_json::from_str(&saved)
            .map_err(|_| StorageError::Schema("Frozen draft snapshot is malformed".into()))?;
        // Submitted metadata adds factual acceptance IDs; the frozen source must match.
        if let Some(metadata) = saved.get_mut("_submission").and_then(Value::as_object_mut) {
            metadata.remove("message_id");
            metadata.remove("accepted_thread_id");
        }
        if !matches!(state.as_str(), "submitting" | "submitted") || saved != body {
            return Err(crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::OperationConflict,
                "Submission operation was already prepared with different draft content",
            )
            .into());
        }
        tx.commit()?;
        return Ok(());
    }
    if let Some(thread) = thread {
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
            [thread],
            |r| r.get(0),
        )?;
        if !live {
            return Err(crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::ThreadNotFound,
                "Draft target Thread is unavailable",
            )
            .into());
        }
    }
    crate::gc::require_capacity_tx(&tx, config).map_err(StorageError::from)?;
    tx.execute("INSERT INTO draft(id,thread_id,revision,body_json,state,updated_at)VALUES(?1,?2,?3,?4,'submitting',?5)",params![submission.draft_id,thread,submission.revision as i64,body.to_string(),chrono::Utc::now().to_rfc3339()])?;
    tx.commit()?;
    Ok(())
}
pub(super) fn submit_draft_tx(
    tx: &Transaction<'_>,
    operation: &str,
    submission: &super::db::DraftSubmission,
    accepted: &Value,
    at: &str,
    config: &Config,
) -> Result<(), StorageError> {
    let message = accepted
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Accepted draft has no message ID"))?;
    let thread = accepted
        .get("thread_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Accepted draft has no Thread ID"))?;
    let saved: Option<String> = tx
        .query_row(
            "SELECT body_json FROM draft WHERE id=?1",
            [&submission.draft_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(saved) = saved else {
        return Err(invalid("Frozen draft submission is missing"));
    };
    let mut saved: Value = serde_json::from_str(&saved)
        .map_err(|_| StorageError::Schema("Frozen draft snapshot is malformed".into()))?;
    let mut source = saved.clone();
    source
        .as_object_mut()
        .ok_or_else(|| invalid("Frozen draft snapshot must be an object"))?
        .remove("_submission");
    let mut supplied = submission.snapshot.clone();
    supplied
        .as_object_mut()
        .ok_or_else(|| invalid("Frozen draft snapshot must be an object"))?
        .remove("_submission");
    if source != supplied {
        return Err(crate::protocol::ApiError::new(
            crate::protocol::ErrorCode::OperationConflict,
            "Frozen draft content changed before acceptance",
        )
        .into());
    }
    if !saved.get("_submission").is_some_and(Value::is_object) {
        return Err(StorageError::Schema(
            "Frozen submission metadata is malformed".into(),
        ));
    }
    saved["_submission"]["message_id"] = json!(message);
    saved["_submission"]["accepted_thread_id"] = json!(thread);
    let saved = saved.to_string();
    if saved.len() as u64 > frozen_serialized_budget(config) {
        return Err(crate::protocol::ApiError::new(
            crate::protocol::ErrorCode::InputLimit,
            "Accepted frozen draft serialized metadata exceeds its bounded storage allowance",
        )
        .into());
    }
    let changed=tx.execute("UPDATE draft SET state='submitted',thread_id=?2,updated_at=?3,body_json=?4 WHERE id=?1 AND state='submitting' AND revision=?5 AND json_extract(body_json,'$._submission.operation_id')=?6 AND json_extract(body_json,'$._submission.source_draft_id')=?7 AND json_extract(body_json,'$._submission.source_revision')=?5",params![submission.draft_id,thread,at,saved,submission.revision as i64,operation,submission.source_draft_id])?;
    if changed != 1 {
        return Err(crate::protocol::ApiError::new(
            crate::protocol::ErrorCode::OperationConflict,
            "Frozen draft submission is missing or changed; acceptance was rolled back",
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
