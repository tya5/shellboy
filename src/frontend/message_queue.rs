//! Bounded current-input reads and source-backed draft restoration.
use super::*;
use rusqlite::{Connection, OptionalExtension};
use std::sync::atomic::{AtomicU64, Ordering};
static READ_REVISION: AtomicU64 = AtomicU64::new(1);
pub(super) fn read_revision() -> u64 {
    READ_REVISION.fetch_add(1, Ordering::Relaxed)
}
pub(super) fn query(
    db: &Connection,
    thread: &str,
) -> Result<PendingMessages, crate::storage::StorageError> {
    const ELIGIBLE: &str = "r.target_thread_id=?1 AND r.state IN ('accepted','held') AND r.path_admission<>'admitted' AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=r.target_thread_id AND p.node_id=r.message_node_id)";
    let total: i64 = db.query_row(
        &format!("SELECT count(*) FROM receipt r WHERE {ELIGIBLE}"),
        [thread],
        |row| row.get(0),
    )?;
    // SQLite returns bounded text fragments; never load 128 full message blobs
    // merely to draw a preview. Full canonical content stays source-addressable.
    let mut stmt=db.prepare(&format!("SELECT r.message_node_id,r.client_operation_id,r.sender_json,r.created_at,r.state,r.hold_reason,r.path_admission,COALESCE(json_extract(r.result_json,'$.now'),0),COALESCE((SELECT substr(group_concat(fragment,''),1,513) FROM (SELECT substr(json_extract(j.value,'$.text'),1,513) AS fragment FROM json_each(n.payload_json,'$.content') j WHERE json_extract(j.value,'$.type') IN ('input_text','output_text') LIMIT 128)),''),(SELECT count(*) FROM attachment_ref a WHERE a.source_node_id=n.id) FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE {ELIGIBLE} ORDER BY r.created_at,r.rowid LIMIT 128"))?;
    let items = stmt
        .query_map([thread], |row| {
            let sender: String = row.get(2)?;
            let sender_thread = serde_json::from_str::<Value>(&sender).ok().and_then(|v| {
                v.get("thread_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            let excerpt: String = row.get(8)?;
            Ok(PendingMessage {
                message_id: row.get(0)?,
                operation_id: row.get(1)?,
                sender_thread,
                created_at: row.get(3)?,
                held: row.get::<_, String>(4)? == "held",
                hold_reason: row.get(5)?,
                uncertain: row.get::<_, String>(6)? == "unknown",
                now: row.get(7)?,
                excerpt_truncated: excerpt.chars().count() > 512,
                excerpt: excerpt.chars().take(512).collect(),
                attachment_count: row.get::<_, i64>(9)? as u64,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut submitted=db.prepare("SELECT operation_id,source_draft_id,source_revision,message_id FROM (SELECT json_extract(body_json,'$._submission.operation_id') AS operation_id,json_extract(body_json,'$._submission.source_draft_id') AS source_draft_id,json_extract(body_json,'$._submission.source_revision') AS source_revision,json_extract(body_json,'$._submission.message_id') AS message_id,updated_at AS accepted_at FROM draft WHERE thread_id=?1 AND state='submitted' UNION ALL SELECT client_operation_id,json_extract(result_json,'$.draft_submission.source_draft_id'),json_extract(result_json,'$.draft_submission.source_revision'),message_node_id,created_at FROM receipt WHERE target_thread_id=?1 AND json_extract(result_json,'$.draft_submission.operation_id')=client_operation_id AND json_extract(result_json,'$.draft_submission.message_id')=message_node_id) GROUP BY operation_id,source_draft_id,source_revision,message_id ORDER BY max(accepted_at) DESC,operation_id DESC LIMIT 128")?;
    let submitted = submitted
        .query_map([thread], |row| {
            Ok(SubmittedMessage {
                operation_id: row.get(0)?,
                source_draft_id: row.get(1)?,
                revision: row.get::<_, i64>(2)? as u64,
                message_id: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PendingMessages {
        items,
        total: total as u64,
        submitted,
    })
}
pub(super) fn submission_snapshot(
    snapshot: Option<Box<DraftSnapshot>>,
    draft_id: &str,
    revision: u64,
    text: &str,
    attachments: &[UiAttachment],
    edit: Option<(&str, &str)>,
    limit: u64,
) -> Result<DraftSnapshot, ApiError> {
    let snapshot = snapshot
        .map(|snapshot| *snapshot)
        .unwrap_or_else(|| DraftSnapshot {
            draft_id: draft_id.into(),
            revision,
            text: text.into(),
            cursor: text.len(),
            attachments: attachments.to_vec(),
            edit: edit.map(|(thread, node)| EditRequestTarget {
                thread_id: thread.into(),
                node_id: node.into(),
                run_id: None,
            }),
            ..Default::default()
        });
    if snapshot.draft_id != draft_id
        || snapshot.revision != revision
        || snapshot.text != text
        || snapshot.attachments != attachments
        || snapshot
            .edit
            .as_ref()
            .map(|edit| (edit.thread_id.as_str(), edit.node_id.as_str()))
            != edit
    {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Submission snapshot does not match the sent draft; retain the original draft",
        ));
    }
    validate_snapshot_positions(&snapshot, limit)?;
    Ok(snapshot)
}
pub(super) async fn archives(
    control: &Control,
    thread: Option<String>,
) -> Result<Vec<(String, String)>, ApiError> {
    let submission_fence = crate::control::submission_retention::fence(
        "json_extract(current.body_json,'$._submission.source_draft_id')",
        "json_extract(current.body_json,'$._submission.source_revision')",
    );
    let result=control.store.database.call(move|db|{
        let mut stmt=db.prepare(&format!("SELECT id,json_extract(body_json,'$.kind'),json_extract(body_json,'$.source_message_id'),substr(json_extract(body_json,'$.text'),1,80) FROM draft current WHERE state IN ('archived','submitting') AND (current.thread_id IS NULL OR EXISTS(SELECT 1 FROM thread t WHERE t.id=current.thread_id AND t.deleted_at IS NULL)) AND (thread_id=?1 OR thread_id IS NULL) AND (state<>'submitting' OR NOT {submission_fence}) ORDER BY updated_at DESC,id DESC LIMIT 128"))?;
        let rows=stmt.query_map([thread],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,Option<String>>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
        let result=rows.into_iter().map(|(id,kind,source,text)|{let label=if kind.as_deref()==Some("cancelled_message") {format!("取消した入力 · {}",source.as_deref().unwrap_or("不明"))} else {format!("未確定の送信 · {}",text.as_deref().unwrap_or(""))};(id,label)}).collect();
        Ok(result)
    }).await.map_err(storage_error);
    checked_storage(control, result).await
}
pub(super) async fn restore_archive(
    control: &Control,
    id: String,
) -> Result<DraftSnapshot, ApiError> {
    let result = restore_archive_inner(control, id).await;
    checked_storage(control, result).await
}
async fn restore_archive_inner(control: &Control, id: String) -> Result<DraftSnapshot, ApiError> {
    let submission_fence = crate::control::submission_retention::fence(
        "json_extract(current.body_json,'$._submission.source_draft_id')",
        "json_extract(current.body_json,'$._submission.source_revision')",
    );
    let value=control.store.database.call(move|db|Ok(db.query_row(&format!("SELECT body_json FROM draft current WHERE id=?1 AND state IN ('archived','submitting') AND (current.thread_id IS NULL OR EXISTS(SELECT 1 FROM thread t WHERE t.id=current.thread_id AND t.deleted_at IS NULL)) AND (state<>'submitting' OR NOT {submission_fence})"),[id],|r|r.get::<_,String>(0)).optional()?)).await.map_err(storage_error)?.ok_or_else(||ApiError::new(ErrorCode::TargetUnavailable,"Restorable draft not found"))?;
    let value: Value = serde_json::from_str(&value)
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Archived draft is malformed"))?;
    let snapshot = if value["kind"] == "cancelled_message" {
        let source = value["source_message_id"]
            .as_str()
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidState, "Archived source is missing"))?;
        let (text, attachments) = restore_snapshot(control, source.into()).await?;
        let operation = value["source_operation_id"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let frozen=control.store.database.call(move|db|Ok(db.query_row("SELECT body_json FROM draft WHERE state='submitted' AND json_extract(body_json,'$._submission.operation_id')=?1 LIMIT 1",[operation],|row|row.get::<_,String>(0)).optional()?)).await.map_err(storage_error)?;
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
            .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Frozen draft is malformed"))?
    };
    validate_restore(&snapshot, control.store.config.limits.draft_bytes)?;
    Ok(snapshot)
}
fn validate_snapshot_positions(snapshot: &DraftSnapshot, limit: u64) -> Result<(), ApiError> {
    if snapshot.text.len() as u64 > limit {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Draft exceeds the current input budget",
        ));
    }
    if snapshot.cursor > snapshot.text.len()
        || !snapshot.text.is_char_boundary(snapshot.cursor)
        || snapshot.folds.iter().any(|fold| {
            fold.start > fold.end
                || fold.end > snapshot.text.len()
                || !snapshot.text.is_char_boundary(fold.start)
                || !snapshot.text.is_char_boundary(fold.end)
        })
    {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            "Saved draft positions are malformed",
        ));
    }
    Ok(())
}
fn validate_restore(snapshot: &DraftSnapshot, limit: u64) -> Result<(), ApiError> {
    validate_snapshot_positions(snapshot, limit)?;
    crate::ui::draft::decode(&snapshot.text, snapshot.attachments.clone()).map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidState,
            "Saved attachment order is malformed",
        )
    })?;
    Ok(())
}
pub(super) async fn restore(
    control: &Control,
    updates: &dyn UpdateTarget,
    target: DraftRecoveryTarget,
    id: String,
) {
    let (draft, message) = match restore_archive(control, id).await {
        Ok(draft) => (
            Some(Box::new(draft)),
            "復元元は /restore list に保持しています。入力欄が空で変更されていなければ復元します"
                .into(),
        ),
        Err(error) => (None, format!("復元できません: {error}")),
    };
    update(
        updates,
        UiUpdate::DraftRecovery {
            target,
            draft,
            message,
            notification_id: None,
        },
    )
    .await;
}
fn append_restored(combined: &mut DraftSnapshot, mut part: DraftSnapshot, limit: u64) -> bool {
    let separator = if combined.text.is_empty() && combined.attachments.is_empty() {
        ""
    } else {
        "\n\n"
    };
    let offset = combined.text.len() + separator.len();
    if offset.saturating_add(part.text.len()) as u64 > limit {
        return false;
    }
    combined.text.push_str(separator);
    combined.text.push_str(&part.text);
    for reference in &mut part.attachments {
        if let Some(position) = &mut reference.position {
            *position += offset;
        }
    }
    for fold in &mut part.folds {
        fold.start += offset;
        fold.end += offset;
    }
    combined.attachments.extend(part.attachments);
    combined.folds.extend(part.folds);
    combined.cursor = combined.text.len();
    combined.literal_paste = true;
    validate_restore(combined, limit).is_ok()
        && serde_json::to_vec(combined).is_ok_and(|body| {
            body.len() as u64
                <= limit
                    .saturating_mul(6)
                    .saturating_add(MAX_FRAME_BYTES as u64)
        })
}
pub(super) async fn cancel(
    control: &Arc<Control>,
    updates: &dyn UpdateTarget,
    target: DraftRecoveryTarget,
    ids: Vec<String>,
) {
    let operation = operation(
        control,
        Command::MessageCancel(MessageCancelArgs { message_ids: ids }),
    );
    let notification_id = operation.operation_id.clone();
    let response = control.request(operation).await;
    let result = if response.ok {
        serde_json::from_value::<MessageCancelResult>(response.result.unwrap_or(Value::Null))
            .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Invalid cancellation reply"))
    } else {
        Err(response.error.unwrap_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidState,
                "Cancellation failed without details",
            )
        }))
    };
    let mut result = match result {
        Ok(result) => result,
        Err(error) => {
            update(
                updates,
                UiUpdate::DraftRecovery {
                    target,
                    draft: None,
                    message: format!("取消できません: {error}"),
                    notification_id: Some(notification_id),
                },
            )
            .await;
            return;
        }
    };
    let ids = result
        .items
        .iter()
        .map(|item| item.message_id.clone())
        .collect::<Vec<_>>();
    let ranks=control.store.database.call(move|db|{
        let ids=serde_json::to_string(&ids).map_err(|_|crate::storage::StorageError::Schema("Cannot order cancelled sources".into()))?;
        let mut query=db.prepare("SELECT message_node_id FROM receipt WHERE message_node_id IN (SELECT value FROM json_each(?1)) ORDER BY created_at,rowid")?;
        Ok(query.query_map([ids],|row|row.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?)
    }).await;
    let ranks = checked_storage(control, ranks.map_err(storage_error)).await;
    let ordered = ranks.is_ok();
    if let Ok(ranks) = ranks {
        result.items.sort_by_key(|item| {
            ranks
                .iter()
                .position(|id| id == &item.message_id)
                .unwrap_or(usize::MAX)
        });
    }
    let mut combined = DraftSnapshot::default();
    let mut newly_cancelled = 0;
    let mut restorable = ordered;
    for item in &result.items {
        if item.outcome != MessageCancelOutcome::Cancelled {
            continue;
        }
        newly_cancelled += 1;
        let Some(id) = item.archive_draft_id.clone() else {
            restorable = false;
            continue;
        };
        if !restorable {
            continue;
        }
        match restore_archive(control, id).await {
            Ok(part) => {
                restorable =
                    append_restored(&mut combined, part, control.store.config.limits.draft_bytes)
            }
            Err(_) => restorable = false,
        }
    }
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for item in &result.items {
        let label = match item.outcome {
            MessageCancelOutcome::Cancelled => "取消",
            MessageCancelOutcome::AlreadyCancelled => "取消済み",
            MessageCancelOutcome::AlreadyAdmitted => "すでに会話へ反映済み",
            MessageCancelOutcome::NotFound => "見つかりません",
            MessageCancelOutcome::Forbidden => "権限なし",
            MessageCancelOutcome::Unavailable => "対象を利用できません",
        };
        *counts.entry(label).or_default() += 1;
    }
    let outcomes = counts
        .into_iter()
        .map(|(label, count)| format!("{label} {count}件"))
        .collect::<Vec<_>>()
        .join(" · ");
    let message = if newly_cancelled == 0 {
        outcomes
    } else {
        format!("{outcomes} · /restore list に保存済み。編集中の入力は保持します")
    };
    update(
        updates,
        UiUpdate::DraftRecovery {
            target,
            draft: (restorable && newly_cancelled > 0).then(|| Box::new(combined)),
            message,
            notification_id: Some(notification_id),
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE receipt(message_node_id TEXT,client_operation_id TEXT,sender_json TEXT,created_at TEXT,state TEXT,hold_reason TEXT,path_admission TEXT,result_json TEXT,target_thread_id TEXT); CREATE TABLE node(id TEXT,payload_json TEXT); CREATE TABLE thread_path(thread_id TEXT,node_id TEXT); CREATE TABLE attachment_ref(source_node_id TEXT); CREATE TABLE draft(id TEXT,thread_id TEXT,state TEXT,updated_at TEXT,body_json TEXT);").unwrap();
        db
    }
    fn input(
        db: &Connection,
        id: &str,
        state: &str,
        admission: &str,
        text: &str,
        sender: Value,
        now: bool,
    ) {
        let payload =
            serde_json::json!({"role":"user","content":[{"type":"input_text","text":text}]})
                .to_string();
        db.execute("INSERT INTO node VALUES(?1,?2)", params![id, payload])
            .unwrap();
        db.execute(
            "INSERT INTO receipt VALUES(?1,?2,?3,'2026-10-30T00:00:00Z',?4,?5,?6,?7,'thread')",
            params![
                id,
                format!("fixture:{id}"),
                sender.to_string(),
                state,
                (state == "held").then_some("terminal_before_path_admission"),
                admission,
                serde_json::json!({"now":now}).to_string()
            ],
        )
        .unwrap();
    }
    #[test]
    fn queue_uses_saved_sender_admission_membership_and_original_receipt_order() {
        let db = fixture();
        input(
            &db,
            "z-first",
            "accepted",
            "unadmitted",
            "first",
            Value::Null,
            false,
        );
        input(
            &db,
            "a-second",
            "held",
            "unknown",
            "second",
            serde_json::json!({"thread_id":"sender-thread"}),
            true,
        );
        input(
            &db,
            "removed-but-admitted",
            "accepted",
            "admitted",
            "removed",
            Value::Null,
            false,
        );
        input(
            &db,
            "selected",
            "accepted",
            "unadmitted",
            "selected",
            Value::Null,
            false,
        );
        input(
            &db,
            "cancelled",
            "cancelled",
            "unadmitted",
            "cancelled",
            Value::Null,
            false,
        );
        db.execute("INSERT INTO thread_path VALUES('thread','selected')", [])
            .unwrap();
        let before = query(&db, "thread").unwrap();
        assert_eq!(
            before
                .items
                .iter()
                .map(|item| item.message_id.as_str())
                .collect::<Vec<_>>(),
            ["z-first", "a-second"]
        );
        assert_eq!(before.total, 2);
        assert!(before.items[0].sender_thread.is_none());
        assert_eq!(
            before.items[1].sender_thread.as_deref(),
            Some("sender-thread")
        );
        assert!(before.items[1].now && before.items[1].held && before.items[1].uncertain);
        let tx = db.unchecked_transaction().unwrap();
        tx.execute("INSERT INTO thread_path VALUES('thread','z-first')", [])
            .unwrap();
        let after = query(&tx, "thread").unwrap();
        let selected: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM thread_path WHERE node_id='z-first')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(selected && after.items.iter().all(|item| item.message_id != "z-first"));
        tx.commit().unwrap();
    }
    #[test]
    fn queue_bounds_rows_and_unicode_excerpts_without_hiding_the_total() {
        let db = fixture();
        for index in 0..130 {
            input(
                &db,
                &format!("message-{index}"),
                "accepted",
                "unadmitted",
                &"日".repeat(700),
                Value::Null,
                false,
            );
        }
        let queue = query(&db, "thread").unwrap();
        assert_eq!(queue.total, 130);
        assert_eq!(queue.items.len(), 128);
        assert!(
            queue
                .items
                .iter()
                .all(|item| item.excerpt.chars().count() == 512 && item.excerpt_truncated)
        );
        assert_eq!(queue.items[127].message_id, "message-127");
    }
    #[test]
    fn frozen_submission_identity_and_content_cannot_diverge_from_sent_input() {
        let snapshot = DraftSnapshot {
            draft_id: "source".into(),
            revision: 7,
            text: "A".into(),
            cursor: 1,
            ..Default::default()
        };
        for (draft, revision, text) in [
            ("different", 7, "A"),
            ("source", 8, "A"),
            ("source", 7, "B"),
        ] {
            assert!(
                submission_snapshot(
                    Some(Box::new(snapshot.clone())),
                    draft,
                    revision,
                    text,
                    &[],
                    None,
                    4096
                )
                .is_err()
            );
        }
        assert_eq!(
            submission_snapshot(
                Some(Box::new(snapshot.clone())),
                "source",
                7,
                "A",
                &[],
                None,
                4096
            )
            .unwrap(),
            snapshot
        );
    }
    #[test]
    fn tiny_text_budget_does_not_count_snapshot_json_metadata_as_user_text() {
        let mut combined = DraftSnapshot::default();
        assert!(append_restored(
            &mut combined,
            DraftSnapshot {
                text: "hi".into(),
                ..Default::default()
            },
            2
        ));
        assert_eq!(combined.text, "hi");
        assert!(!append_restored(
            &mut combined,
            DraftSnapshot {
                text: "x".into(),
                ..Default::default()
            },
            2
        ));
    }
    #[test]
    fn merged_restore_preserves_attachment_positions_folds_and_budget() {
        let mut combined = DraftSnapshot::default();
        let first = DraftSnapshot {
            text: "first".into(),
            cursor: 5,
            folds: vec![PasteFold {
                id: 1,
                start: 0,
                end: 5,
                expanded: false,
            }],
            ..Default::default()
        };
        assert!(append_restored(&mut combined, first, 4096));
        let part = DraftSnapshot {
            text: format!("前{}後", crate::ui::draft::MARKER),
            cursor: 9,
            attachments: vec![UiAttachment {
                id: "attachment".into(),
                path: PathBuf::from("/synthetic/file"),
                kind: "text".into(),
                state: "saved".into(),
                position: Some(3),
            }],
            ..Default::default()
        };
        assert!(append_restored(&mut combined, part, 4096));
        assert_eq!(combined.attachments[0].position, Some("first\n\n前".len()));
        assert_eq!(combined.folds[0].end, 5);
        validate_restore(&combined, 4096).unwrap();
        assert!(!append_restored(
            &mut combined,
            DraftSnapshot {
                text: "x".repeat(5000),
                ..Default::default()
            },
            4096
        ));
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_held_input_is_listed_cancelled_and_restored_in_receipt_order_across_restart() {
        use crate::{app::DataOwner, ui::UiState};
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let path = root.path().join("source.txt");
        std::fs::write(&path, "synthetic attachment").unwrap();
        let owner = DataOwner::start(
            Some(config.clone()),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let cwd = root.path().to_string_lossy().into_owned();
        let file = path.clone();
        owner.db.call(move|db| {
            let tx=db.transaction()?;
            tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread',?1,'','fixture','fixture','2026-10-30T00:00:00Z')",[cwd])?;
            tx.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('old-run','thread','stopped','2026-10-30T00:00:00Z','2026-10-30T00:00:00Z')",[])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('old-turn','thread','old-run','stopped','2026-10-30T00:00:00Z','2026-10-30T00:00:00Z')",[])?;
            for (id,content) in [("z-first",serde_json::json!([{"type":"input_text","text":"before"},{"type":"attachment_ref","attachment_id":"file-ref"},{"type":"input_text","text":"after"}])),("a-second",serde_json::json!([{"type":"input_text","text":"second"}]))] {
                let payload=serde_json::json!({"role":"user","content":content}).to_string();let hash=crate::digest::sha256_hex(payload.as_bytes());
                tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,'old-turn','user','2026-10-30T00:00:00Z',?2,?3,?4)",params![id,payload,payload.len()as i64,hash])?;
                tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,updated_at,hold_reason,path_admission)VALUES(?1,?2,'null','thread',?1,'old-run','old-turn','held','2026-10-30T00:00:00Z','2026-10-30T00:00:00Z','uncertain_path_admission','unknown')",params![id,format!("old-instance:{id}")])?;
            }
            tx.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('file-ref','z-first',?1,'file','external','2026-10-30T00:00:00Z')",[file.to_string_lossy().as_ref()])?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let pending = owner.db.call(|db| query(db, "thread")).await.unwrap();
        assert_eq!(
            pending
                .items
                .iter()
                .map(|item| item.message_id.as_str())
                .collect::<Vec<_>>(),
            ["z-first", "a-second"]
        );
        assert!(pending.items.iter().all(|item| item.held && item.uncertain));
        let mut ui = UiState::new(&owner.loaded.config, root.path().into());
        ui.select_thread("thread".into());
        let generation = ui.view().generation;
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation,
            nodes: vec![],
            pending,
            read_revision: 1,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 0,
            total_nodes: 0,
        });
        ui.insert("/queue", false);
        ui.event(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
        ));
        let (send, receive) = std::sync::mpsc::sync_channel(4);
        let target = DraftRecoveryTarget {
            draft_id: ui.view().draft.draft_id.clone(),
            revision: ui.view().draft.revision,
            generation,
            thread_id: Some("thread".into()),
        };
        // Deliberately reverse explicit request IDs: auto-restore must still
        // use original receipt order, including equal timestamps.
        tokio::time::timeout(
            Duration::from_secs(60),
            cancel(
                &control,
                &send,
                target,
                vec!["a-second".into(), "z-first".into()],
            ),
        )
        .await
        .unwrap();
        let result = receive.try_recv().unwrap();
        let UiUpdate::DraftRecovery {
            draft: Some(snapshot),
            ..
        } = &result
        else {
            panic!("unknown held input must have an explicit restore path");
        };
        assert_eq!(
            snapshot.text,
            format!("before{}after\n\nsecond", crate::ui::draft::MARKER)
        );
        assert_eq!(snapshot.attachments.len(), 1);
        assert_eq!(snapshot.attachments[0].path, path);
        assert_eq!(snapshot.attachments[0].position, Some(6));
        ui.apply(result);
        assert_eq!(
            ui.view().draft.text,
            format!("before{}after\n\nsecond", crate::ui::draft::MARKER)
        );
        let listed = archives(&control, Some("thread".into())).await.unwrap();
        assert_eq!(listed.len(), 2);
        let pending = owner.db.call(|db| query(db, "thread")).await.unwrap();
        assert_eq!(pending.total, 0);
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation,
            nodes: vec![],
            pending,
            read_revision: 2,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 0,
            total_nodes: 0,
        });
        assert_eq!(ui.view().pending_messages.total, 0);
        owner
            .db
            .call(|db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM thread_path WHERE thread_id='thread'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )?,
                    0,
                    "cancel/restore does not deliver unread input"
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM model_attempt", [], |row| row
                        .get::<_, i64>(0))?,
                    0,
                    "explicit cancellation never wakes a model"
                );
                Ok(())
            })
            .await
            .unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let restored = restore_archive(&control, "cancelled-message:z-first".into())
            .await
            .unwrap();
        assert_eq!(
            restored.text,
            format!("before{}after", crate::ui::draft::MARKER)
        );
        assert_eq!(restored.attachments[0].position, Some(6));
        assert_eq!(
            archives(&control, Some("thread".into()))
                .await
                .unwrap()
                .len(),
            2
        );
        // Simulate the interval after logical Thread deletion but before the
        // frontend removes its draft rows. A live fork still retains the source.
        owner.db.call(|db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('fork','/fixture','','fixture','fixture','now')",[])?;
            db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('fork',0,'z-first')",[])?;
            db.execute("UPDATE thread SET cursor_node_id='z-first' WHERE id='fork'",[])?;
            db.execute("UPDATE thread SET deleted_at='2026-10-30T00:00:01Z' WHERE id='thread'",[])?;
            assert_eq!(db.query_row("SELECT count(*) FROM draft WHERE state='archived'",[],|row|row.get::<_,i64>(0))?,2);
            Ok(())
        }).await.unwrap();
        assert!(
            archives(&control, Some("thread".into()))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            restore_archive(&control, "cancelled-message:z-first".into())
                .await
                .unwrap_err()
                .code,
            ErrorCode::TargetUnavailable
        );
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_copy_is_retired_but_text_and_attachment_restore_without_sending() {
        let (root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let path = root.path().join("retention-file.txt");
        std::fs::write(&path, b"fixture attachment").unwrap();
        let file = path.to_string_lossy().into_owned();
        owner.db.call(move|db| {
            let tx=db.transaction()?;
            let body=json!({"role":"user","content":[{"type":"input_text","text":"before"},{"type":"attachment_ref","attachment_id":"retention-file"},{"type":"input_text","text":"after"}]}).to_string();
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES('retention-message','user','0',?1,?2,?3)",params![body,body.len()as i64,crate::digest::sha256_hex(body.as_bytes())])?;
            tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,path_admission,created_at,result_json)VALUES('retention-message','retention-op','null','thread','retention-message','held','unadmitted','0','{}')",[])?;
            tx.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('retention-file','retention-message',?1,'file','external','0')",[file])?;
            let body=json!({"draft_id":"retention-composer","revision":7,"text":"beforeafter","attachments":[],"_submission":{"operation_id":"retention-op","source_draft_id":"retention-composer","source_revision":7,"message_id":"retention-message","accepted_thread_id":"thread"}}).to_string();
            tx.execute("INSERT INTO draft VALUES(?1,'thread',7,?2,'submitted','0')",params![crate::control::db::DraftSubmission::id_for_operation("retention-op"),body])?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let before = owner
            .db
            .call(|db| {
                Ok(db.query_row("SELECT count(*) FROM model_attempt", [], |r| {
                    r.get::<_, i64>(0)
                })?)
            })
            .await
            .unwrap();
        let request = Request::new(
            format!("{}:retention-cancel", owner.instance_id),
            Command::MessageCancel(MessageCancelArgs {
                message_ids: vec!["retention-message".into()],
            }),
        );
        let response = control.request(request).await;
        assert!(response.ok, "{:?}", response.error);
        let archive = response.result.unwrap()["items"][0]["archive_draft_id"]
            .as_str()
            .unwrap()
            .to_owned();
        control
            .gc
            .collect(StorageCategory::History, chrono::Utc::now())
            .await
            .unwrap();
        let snapshot = restore_archive_inner(&control, archive.clone())
            .await
            .unwrap();
        assert_eq!(
            snapshot.text,
            format!("before{}after", crate::ui::draft::MARKER)
        );
        assert_eq!(snapshot.attachments.len(), 1);
        assert_eq!(snapshot.attachments[0].position, Some("before".len()));
        assert_eq!(snapshot.attachments[0].path, path);
        owner
            .db
            .call(move |db| {
                assert!(!db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM draft WHERE state='submitted')",
                    [],
                    |r| r.get::<_, bool>(0)
                )?);
                assert!(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM draft WHERE id=?1 AND state='archived')",
                    [archive],
                    |r| r.get::<_, bool>(0)
                )?);
                assert_eq!(
                    db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                        .get::<_, i64>(0))?,
                    before
                );
                assert!(
                    query(db, "thread")?
                        .submitted
                        .iter()
                        .any(|s| s.source_draft_id == "retention-composer" && s.revision == 7)
                );
                Ok(())
            })
            .await
            .unwrap();
        // Simulate a crash-before-ack editable copy and an older unfinished submission.
        owner.db.call(|db| {
            let snapshot=serde_json::to_string(&DraftSnapshot{draft_id:"retention-composer".into(),revision:7,text:"stale unsent copy".into(),..Default::default()}).unwrap();
            db.execute("INSERT INTO draft VALUES('retention-composer','thread',7,?1,'editing','later')",[snapshot])?;
            let snapshot=json!({"draft_id":"retention-composer","revision":6,"text":"older unfinished","attachments":[],"_submission":{"source_draft_id":"retention-composer","source_revision":6}}).to_string();
            db.execute("INSERT INTO draft VALUES('older-submitting','thread',6,?1,'submitting','before')",[snapshot])?;Ok(())
        }).await.unwrap();
        let (updates, incoming) = std::sync::mpsc::sync_channel(128);
        crate::frontend::select_thread(&control, &updates, "thread", 7, 0, None, 80)
            .await
            .unwrap();
        assert!(
            incoming
                .try_iter()
                .any(|update| matches!(update, UiUpdate::Thread { draft: None, .. })),
            "sent editable content must not restore"
        );
        assert!(
            !archives(&control, Some("thread".into()))
                .await
                .unwrap()
                .iter()
                .any(|(id, _)| id == "older-submitting")
        );
        assert!(
            restore_archive_inner(&control, "older-submitting".into())
                .await
                .is_err()
        );
        owner.db.call(|db| {
            assert!(db.query_row("SELECT EXISTS(SELECT 1 FROM draft WHERE id='older-submitting' AND state='submitting')",[],|r|r.get::<_,bool>(0))?);
            // Remove the synthetic old editable fixture to observe insert rejection separately.
            db.execute("DELETE FROM draft WHERE id='retention-composer'",[])?;Ok(())
        }).await.unwrap();
        for revision in [7, 8] {
            let draft = DraftSnapshot {
                draft_id: "retention-composer".into(),
                revision,
                text: "late autosave".into(),
                ..Default::default()
            };
            crate::frontend::save_draft(
                &control,
                "thread".into(),
                draft,
                ViewAnchor::default(),
                true,
            )
            .await
            .unwrap();
            let exists=owner.db.call(move|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM draft WHERE id='retention-composer' AND state='editing' AND revision=?1)",[revision as i64],|r|r.get::<_,bool>(0))?)).await.unwrap();
            assert_eq!(exists, revision == 8);
        }
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
