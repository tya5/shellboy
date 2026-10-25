//! Selection snapshot, source validation and runtime pins in one caller transaction.
use super::*;

struct SelectionRow {
    id: String,
    path: String,
    kind: String,
    protected: bool,
    state: String,
    reason: Option<String>,
    managed: Option<String>,
    managed_state: Option<String>,
    hash: Option<String>,
    source_node_id: Option<String>,
    payload: Option<String>,
    previously_included: bool,
}
impl SelectionRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            path: row.get(1)?,
            kind: row.get(2)?,
            protected: row.get(3)?,
            state: row.get(4)?,
            reason: row.get(5)?,
            managed: row.get(6)?,
            managed_state: row.get(7)?,
            hash: row.get(8)?,
            source_node_id: row.get(9)?,
            payload: row.get(10)?,
            previously_included: row.get(11)?,
        })
    }
}
pub(super) struct SelectionSnapshot {
    pub(super) revision: i64,
    pub(super) selected: Vec<Selected>,
    pub(super) pin_ids: Vec<String>,
}
pub(super) fn snapshot_selection_tx(
    tx: &Transaction<'_>,
    thread: &str,
    instance: &str,
    expected_revision: Option<i64>,
) -> Result<SelectionSnapshot, StorageError> {
    let revision =
        crate::history::selection_revision(tx, thread, crate::history::ThreadScope::Live)
            .optional()?
            .ok_or_else(|| {
                StorageError::from(ApiError::new(
                    ErrorCode::ThreadNotFound,
                    "Attachment target Thread was not found or was deleted",
                ))
            })?;
    if expected_revision.is_some_and(|expected| expected != revision) {
        return Err(StorageError::from(stale()));
    }
    let mut statement=tx.prepare("SELECT a.id,a.absolute_path,a.input_kind,s.protected,s.state,s.reason,m.id,m.state,(SELECT z.content_hash FROM attachment_send z JOIN model_attempt ma ON ma.id=z.attempt_id JOIN turn tu ON tu.id=ma.turn_id WHERE z.attachment_id=a.id AND tu.thread_id=s.thread_id AND z.state='sent' AND ma.state='completed' ORDER BY ma.started_at DESC,z.ordinal DESC LIMIT 1),a.source_node_id,n.payload_json,EXISTS(SELECT 1 FROM attachment_send z JOIN model_attempt ma ON ma.id=z.attempt_id JOIN turn tu ON tu.id=ma.turn_id WHERE z.attachment_id=a.id AND tu.thread_id=s.thread_id AND ma.state='completed') FROM attachment_selection s JOIN attachment_ref a ON a.id=s.attachment_id LEFT JOIN managed_file m ON m.absolute_path=a.absolute_path LEFT JOIN node n ON n.id=a.source_node_id WHERE s.thread_id=?1 AND (COALESCE(s.reason,'')<>'context_replacement' OR EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=s.thread_id AND p.node_id=a.source_node_id) OR NOT EXISTS(SELECT 1 FROM attachment_send z JOIN model_attempt ma ON ma.id=z.attempt_id JOIN turn tu ON tu.id=ma.turn_id WHERE z.attachment_id=a.id AND z.state='excluded' AND tu.thread_id=s.thread_id AND ma.state='completed')) AND NOT (s.state='excluded' AND COALESCE(s.reason,'')='context_replacement' AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=s.thread_id AND p.node_id=a.source_node_id) AND n.id IS NOT NULL AND (n.payload_json IS NULL OR n.deleted_reason IS NOT NULL)) AND NOT EXISTS(SELECT 1 FROM attachment_delivery d WHERE d.node_id=a.source_node_id AND d.thread_id=s.thread_id AND d.status<>'applied') ORDER BY s.selected_order,a.id")?;
    let mut selected = Vec::new();
    let mut pins = Vec::new();
    let mut detached_count = 0;
    for row in statement.query_map([&thread], SelectionRow::from_row)? {
        let SelectionRow {
            id,
            path,
            kind,
            protected,
            state,
            reason,
            managed,
            managed_state,
            hash,
            source_node_id,
            payload,
            previously_included,
        } = row?;
        let source_node_id = source_node_id.ok_or_else(|| {
            StorageError::InvalidInput(format!("Attachment {id} source is missing"))
        })?;
        let in_path: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND node_id=?2)",
            params![thread, source_node_id],
            |r| r.get(0),
        )?;
        let detached = !in_path && reason.as_deref() == Some("context_replacement");
        if detached {
            if detached_count >= DEFAULT_LIST_LIMIT {
                continue;
            }
            detached_count += 1;
        }
        let payload: Value =
            serde_json::from_str(payload.as_deref().unwrap_or("null")).map_err(|e| {
                StorageError::InvalidInput(format!("Invalid attachment source payload: {e}"))
            })?;
        let positions: Vec<_> = payload
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, v)| {
                v.get("type").and_then(Value::as_str) == Some("attachment_ref")
                    && v.get("attachment_id").and_then(Value::as_str) == Some(id.as_str())
            })
            .map(|(i, _)| i)
            .collect();
        if positions.len() != 1 {
            return Err(StorageError::InvalidInput(format!(
                "Attachment {id} must have exactly one ordered source reference"
            )));
        }
        if !in_path && !detached {
            let pending_human:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=?1 AND r.message_node_id=?2 AND r.state IN ('accepted','held') AND n.kind='user')",params![thread,source_node_id],|r|r.get(0))?;
            if pending_human {
                continue;
            }
            return Err(StorageError::InvalidInput(format!(
                "Attachment {id} source is outside the selected path"
            )));
        }
        let source_element_index = positions[0];
        let active = state == "active";
        if active && managed.is_some() && managed_state.as_deref() != Some("ready") {
            return Err(StorageError::InvalidInput(
                "Managed attachment is not ready; retry after preparation or remove it".into(),
            ));
        }
        if active && let Some(managed) = &managed {
            let pin = super::id();
            tx.execute("INSERT INTO runtime_pin(id,instance_id,resource_kind,resource_id,generation,created_at)VALUES(?1,?2,'managed_file',?3,?4,?5)",params![pin,instance,managed,revision,now()])?;
            pins.push(pin);
        }
        selected.push(Selected {
            id,
            source_node_id,
            source_element_index,
            path: PathBuf::from(path),
            kind: if kind == "image" {
                AttachmentKind::Image
            } else {
                AttachmentKind::File
            },
            protected,
            active,
            reason,
            previous_hash: hash,
            previously_included,
            detached,
        });
    }
    drop(statement);
    Ok(SelectionSnapshot {
        revision,
        selected,
        pin_ids: pins,
    })
}
