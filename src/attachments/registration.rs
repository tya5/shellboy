//! Atomic attachment reference registration.
use super::*;

/// Register already declared, final source references in the caller's transaction.
/// Never appends to or rewrites the source node payload.
pub fn add_tx(
    tx: &Transaction<'_>,
    plan: &AddPlan,
    source_node_id: Option<&str>,
) -> Result<Vec<AttachmentDetails>, ApiError> {
    let source_node_id = source_node_id.ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidArgument,
            "Committed attachments require a source node",
        )
    })?;
    let payload: String = tx
        .query_row(
            "SELECT payload_json FROM node WHERE id=?1",
            [source_node_id],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    let payload: Value = serde_json::from_str(&payload)
        .map_err(|_| ApiError::new(ErrorCode::InvalidArgument, "Invalid source payload"))?;
    let content = payload.get("content").and_then(Value::as_array).ok_or_else(||ApiError::new(ErrorCode::InvalidArgument,"Source payload must contain its final ordered content before attachment registration"))?;
    let mut attachment_ids = Vec::new();
    let mut unique = HashSet::new();
    for element in content {
        if element.get("type").and_then(Value::as_str) == Some("attachment_ref") {
            let attachment = element
                .get("attachment_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::InvalidArgument,
                        "Invalid final source attachment ID",
                    )
                })?;
            if !unique.insert(attachment) {
                return Err(ApiError::new(
                    ErrorCode::InvalidArgument,
                    "Final source attachment IDs must be unique",
                ));
            }
            attachment_ids.push(attachment);
        }
    }
    if attachment_ids.len() != plan.files.len() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Source payload must declare each planned attachment before registration; existing node content cannot be appended",
        ));
    }
    let revision: Option<i64> = tx
        .query_row(
            "SELECT selection_revision FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [&plan.thread_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    if revision != Some(plan.selection_revision) {
        return Err(stale());
    }
    let first_order:i64=tx.query_row("SELECT COALESCE(MAX(selected_order),-1)+1 FROM attachment_selection WHERE thread_id=?1",[&plan.thread_id],|r|r.get(0)).map_err(sql_error)?;
    let mut result = Vec::new();
    for (offset, file) in plan.files.iter().enumerate() {
        let order = i64::try_from(offset)
            .ok()
            .and_then(|offset| first_order.checked_add(offset))
            .ok_or_else(|| {
                ApiError::new(ErrorCode::InputLimit, "Attachment selection order overflow")
            })?;
        if !file.path.is_absolute() {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Attachment reference must be absolute",
            ));
        }
        if let Some(managed) = &file.managed_id {
            let ready:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM managed_file WHERE id=?1 AND state='ready' AND absolute_path=?2)",params![managed,file.path.to_string_lossy()],|r|r.get(0)).map_err(sql_error)?;
            if !ready {
                return Err(stale());
            }
        }
        let attachment = attachment_ids[offset].to_owned();
        let kind = if file.kind == AttachmentKind::Image {
            "image"
        } else {
            "file"
        };
        // Any reference to our owned path shares its lifecycle, including a
        // path-only reattachment of an existing managed image.
        let managed: Option<(String, String)> = tx
            .query_row(
                "SELECT id,state FROM managed_file WHERE absolute_path=?1",
                [file.path.to_string_lossy().as_ref()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        if managed.as_ref().is_some_and(|(_, state)| state != "ready") {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Managed path is being reclaimed or unavailable; it cannot be reactivated",
            ));
        }
        let managed_id = managed.map(|(id, _)| id);
        let ownership = if managed_id.is_some() {
            "managed_clipboard"
        } else {
            "external"
        };
        tx.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES(?1,?2,?3,?4,?5,?6)",params![attachment,source_node_id,file.path.to_string_lossy(),kind,ownership,now()]).map_err(sql_error)?;
        tx.execute("INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order,revision,protected)VALUES(?1,?2,'active',?3,?4,0,1)",params![plan.thread_id,attachment,now(),order]).map_err(sql_error)?;
        if let Some(managed) = &managed_id {
            tx.execute(
                "UPDATE managed_file SET attachment_id=?2 WHERE id=?1 AND state='ready'",
                params![managed, attachment],
            )
            .map_err(sql_error)?;
        }
        result.push(AttachmentDetails {
            source_node_id: source_node_id.into(),
            delivery_status: delivery_status(tx, &plan.thread_id, source_node_id)
                .map_err(sql_error)?,
            attachment_id: attachment,
            thread_id: plan.thread_id.clone(),
            path: file.path.to_string_lossy().into(),
            kind: file.kind,
            selected: true,
            managed: managed_id.is_some(),
            unavailable_reason: None,
        });
    }
    if !plan.files.is_empty() {
        tx.execute("UPDATE thread SET selection_revision=selection_revision+1,revision=revision+1 WHERE id=?1",[&plan.thread_id]).map_err(sql_error)?;
    }
    Ok(result)
}
