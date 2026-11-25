//! Bounded current-input reads and source-backed draft restoration.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
static READ_REVISION: AtomicU64 = AtomicU64::new(1);
pub(crate) fn read_revision() -> u64 {
    READ_REVISION.fetch_add(1, Ordering::Relaxed)
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
    crate::ui::draft::validate_positions(&snapshot, limit)?;
    Ok(snapshot)
}
const DRAFT_VIEW_LIMIT: u32 = 128;

async fn draft_records(
    control: &Control,
    thread_id: Option<String>,
    kind: crate::protocol::DraftListKind,
) -> Result<Vec<crate::protocol::DraftRecord>, ApiError> {
    let page: crate::protocol::DraftListPage = compose_call(
        control,
        Command::DraftList(crate::protocol::DraftListArgs {
            thread_id,
            kind,
            limit: DRAFT_VIEW_LIMIT,
            cursor: None,
        }),
        None,
    )
    .await?;
    Ok(page.items)
}

pub(super) async fn exact_submission(
    control: &Control,
    operation: &str,
    draft_id: &str,
    revision: u64,
    generation: u64,
) -> Result<(String, UiUpdate), ApiError> {
    use crate::protocol::{DraftGetArgs, DraftRecord, DraftState};
    let saved: DraftState = compose_call(
        control,
        Command::DraftGet(DraftGetArgs {
            thread_id: None,
            operation_id: Some(operation.into()),
        }),
        None,
    )
    .await?;
    match saved.record {
        Some(DraftRecord::Submitted {
            operation_id,
            source_draft_id,
            source_revision,
            thread_id,
            message_id,
            ..
        }) if operation_id == operation
            && source_draft_id == draft_id
            && source_revision == revision =>
        {
            Ok((
                thread_id.clone(),
                UiUpdate::ExactSubmission {
                    operation_id,
                    draft_id: source_draft_id,
                    revision,
                    generation,
                    thread_id,
                    message_id,
                },
            ))
        }
        _ => Err(ApiError::new(
            ErrorCode::InvalidState,
            "Saved submission is not the pending draft revision",
        )),
    }
}
pub(super) async fn archives(
    control: &Control,
    thread: Option<String>,
) -> Result<Vec<(String, String)>, ApiError> {
    use crate::protocol::DraftRecord;
    let language = control.store.config.ui.language;
    let separator = crate::ui::text_width::glyphs::separator();
    draft_records(control, thread, crate::protocol::DraftListKind::Restorable)
        .await?
        .into_iter()
        .map(|record| match record {
            DraftRecord::Cancelled {
                draft_id,
                source_message_id,
            } => Ok((
                draft_id,
                match language {
                    UiLanguage::Ja => format!(
                        "取消した入力{separator}{}",
                        source_message_id.as_deref().unwrap_or("不明")
                    ),
                    UiLanguage::En => format!(
                        "Cancelled input{separator}{}",
                        source_message_id.as_deref().unwrap_or("unknown")
                    ),
                },
            )),
            DraftRecord::Unconfirmed { draft_id, excerpt } => Ok((
                draft_id,
                match language {
                    UiLanguage::Ja => format!("未確定の送信{separator}{excerpt}"),
                    UiLanguage::En => format!("Unconfirmed send{separator}{excerpt}"),
                },
            )),
            DraftRecord::Submitted { .. } => Err(ApiError::new(
                ErrorCode::ProtocolMismatch,
                "Invalid restorable draft record",
            )),
        })
        .collect()
}
pub(super) async fn restore_archive(
    control: &Control,
    id: String,
) -> Result<DraftSnapshot, ApiError> {
    compose_call(
        control,
        Command::DraftRestore(crate::protocol::DraftRestoreArgs { archive_id: id }),
        None,
    )
    .await
}
fn validate_restore(snapshot: &DraftSnapshot, limit: u64) -> Result<(), ApiError> {
    crate::ui::draft::validate_positions(snapshot, limit)?;
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
    let language = control.store.config.ui.language;
    let (draft, message) = match restore_archive(control, id).await {
        Ok(draft) => (
            Some(Box::new(draft)),
            i18n::local!(language, "復元元は /restore list に保持しています。入力欄が空で変更されていなければ復元します", "Source kept in /restore list. Restore only into an empty, unchanged composer")
                .into(),
        ),
        Err(error) => (None, match language {UiLanguage::Ja=>format!("復元できません: {error}"),UiLanguage::En=>format!("Could not restore: {error}")}),
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
    let language = control.store.config.ui.language;
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
                    message: match language {
                        UiLanguage::Ja => format!("取消できません: {error}"),
                        UiLanguage::En => format!("Cancellation failed: {error}"),
                    },
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
    let mut counts = std::collections::BTreeMap::<&str, (MessageCancelOutcome, usize)>::new();
    for item in &result.items {
        let label = match item.outcome {
            MessageCancelOutcome::Cancelled => "取消",
            MessageCancelOutcome::AlreadyCancelled => "取消済み",
            MessageCancelOutcome::ReplyCancelled => "返信依頼の取消",
            MessageCancelOutcome::AlreadyAdmitted => "すでに会話へ反映済み",
            MessageCancelOutcome::NotFound => "見つかりません",
            MessageCancelOutcome::Unavailable => "対象を利用できません",
            MessageCancelOutcome::Forbidden => "このメッセージをキャンセルする権限がありません",
        };
        counts.entry(label).or_insert((item.outcome, 0)).1 += 1;
    }
    let outcomes = counts
        .into_iter()
        .map(|(label, (outcome, count))| match language {
            UiLanguage::Ja => format!("{label} {count}件"),
            UiLanguage::En => cancelled_summary(outcome, count),
        })
        .collect::<Vec<_>>()
        .join(crate::ui::text_width::glyphs::separator());
    let message = if newly_cancelled == 0 {
        outcomes
    } else {
        let separator = crate::ui::text_width::glyphs::separator();
        match language {
            UiLanguage::Ja => {
                format!("{outcomes}{separator}/restore list に保存済み。編集中の入力は保持します")
            }
            UiLanguage::En => {
                format!("{outcomes}{separator}Saved in /restore list. Current input is kept")
            }
        }
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

fn cancelled_summary(outcome: MessageCancelOutcome, count: usize) -> String {
    let noun = if count == 1 { "input" } else { "inputs" };
    match outcome {
        MessageCancelOutcome::ReplyCancelled => format!("{count} reply duties cancelled"),
        MessageCancelOutcome::Cancelled => format!("{count} {noun} cancelled"),
        MessageCancelOutcome::AlreadyCancelled => format!("{count} {noun} already cancelled"),
        MessageCancelOutcome::AlreadyAdmitted => {
            format!("{count} {noun} already applied to the conversation")
        }
        MessageCancelOutcome::NotFound => format!("{count} {noun} not found"),
        MessageCancelOutcome::Unavailable => format!("{count} {noun} unavailable"),
        MessageCancelOutcome::Forbidden => format!("{count} {noun} forbidden"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[test]
    fn cancellation_summary_uses_english_for_all_outcomes_and_counts() {
        let cases = [
            (MessageCancelOutcome::Cancelled, "cancelled"),
            (MessageCancelOutcome::AlreadyCancelled, "already cancelled"),
            (
                MessageCancelOutcome::AlreadyAdmitted,
                "already applied to the conversation",
            ),
            (MessageCancelOutcome::NotFound, "not found"),
            (MessageCancelOutcome::Unavailable, "unavailable"),
        ];
        for (outcome, text) in cases {
            assert_eq!(cancelled_summary(outcome, 1), format!("1 input {text}"));
            assert_eq!(cancelled_summary(outcome, 2), format!("2 inputs {text}"));
        }
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
        let (rpc, requests) = tokio::sync::mpsc::channel(8);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(requests));
        let pending = super::native_page::read_queue(&control, "thread")
            .await
            .unwrap();
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
        crate::test_watchdog::watchdog("src/frontend/message_queue.rs: unknown_held_input_is_listed_cancelled_and_restored_in_receipt_order_across_restart",
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
        let pending = super::native_page::read_queue(&control, "thread")
            .await
            .unwrap();
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
        drop(server);
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
        let (rpc, requests) = tokio::sync::mpsc::channel(8);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(requests));
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
        drop(server);
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
    async fn exact_owner_submission_outside_first_page_reconciles_matching_view() {
        use crate::protocol::{DraftListArgs, DraftListKind, DraftListPage, DraftRecord};
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let (root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let mut ui = crate::ui::UiState::new(&crate::config::Config::default(), root.path().into());
        ui.insert("original", false);
        let actions = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let UiAction::Send {
            draft_id,
            revision,
            generation,
            ..
        } = &actions[0]
        else {
            panic!("expected send")
        };
        let operation_id = "old-operation";
        ui.apply(UiUpdate::SendOperation {
            draft_id: draft_id.clone(),
            revision: *revision,
            operation_id: operation_id.into(),
        });
        ui.apply(UiUpdate::Receipt {
            draft_id: draft_id.clone(),
            revision: *revision,
            thread_id: None,
            status: ReceiptStatus::Unknown,
            message: None,
        });
        let source = draft_id.clone();
        let rev = *revision;
        owner.db.call(move |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('old-message','user','0','{}')", [])?;
            let retained = json!({"draft_submission":{"operation_id":operation_id,"source_draft_id":source,"source_revision":rev,"message_id":"old-message"}}).to_string();
            tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,path_admission,created_at,result_json) VALUES('old-message',?1,'null','thread','old-message','applied','admitted','0',?2)", params![operation_id,retained])?;
            for n in 0..128 {
                let id = format!("new-{n}");
                let body = json!({"_submission":{"operation_id":id,"source_draft_id":id,"source_revision":1,"message_id":id}}).to_string();
                tx.execute("INSERT INTO draft VALUES(?1,'thread',1,?2,'submitted','1')", params![crate::control::db::DraftSubmission::id_for_operation(&id),body])?;
            }
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let first: DraftListPage = compose_call(
            &control,
            Command::DraftList(DraftListArgs {
                thread_id: Some("thread".into()),
                kind: DraftListKind::Submitted,
                limit: 128,
                cursor: None,
            }),
            None,
        )
        .await
        .unwrap();
        assert_eq!(first.items.len(), 128);
        assert!(first.next_cursor.is_some());
        assert!(!first.items.iter().any(|item| matches!(item, DraftRecord::Submitted {operation_id: id,..} if id == operation_id)));
        assert!(
            exact_submission(&control, operation_id, draft_id, revision + 1, *generation)
                .await
                .is_err()
        );
        let (_, stale) =
            exact_submission(&control, operation_id, draft_id, *revision, generation + 1)
                .await
                .unwrap();
        ui.apply(stale);
        assert!(ui.view().thread_id.is_none());
        let (thread, exact) =
            exact_submission(&control, operation_id, draft_id, *revision, *generation)
                .await
                .unwrap();
        ui.apply(exact);
        assert_eq!(ui.view().thread_id.as_deref(), Some(thread.as_str()));
        assert!(ui.view().draft.text.is_empty());
        drop(server);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn draft_list_pages_preserve_archive_scope_and_order() {
        use crate::protocol::{DraftListArgs, DraftListKind, DraftListPage, DraftRecord};
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        owner.db.call(|db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('other','/fixture','','fixture','fixture','now')", [])?;
            for (id, thread) in [("a", Some("thread")), ("b", None), ("c", Some("other"))] {
                db.execute("INSERT INTO draft VALUES(?1,?2,1,?3,'archived','same')", params![id, thread, json!({"text":"é".repeat(200)}).to_string()])?;
            }
            Ok(())
        }).await.unwrap();
        let request = DraftListArgs {
            thread_id: Some("thread".into()),
            kind: DraftListKind::Restorable,
            limit: 1,
            cursor: None,
        };
        let list = |args| compose_call::<DraftListPage>(&control, Command::DraftList(args), None);
        let first = list(request.clone()).await.unwrap();
        assert_eq!(first.limit, 1);
        assert_eq!(first.filters.thread_id.as_deref(), Some("thread"));
        assert_eq!(first.filters.kind, DraftListKind::Restorable);
        assert!(
            matches!(first.items.as_slice(), [DraftRecord::Unconfirmed { draft_id, excerpt }] if draft_id == "b" && excerpt == &"é".repeat(80))
        );
        let continuation = DraftListArgs {
            cursor: first.next_cursor.clone(),
            ..request.clone()
        };
        assert!(continuation.cursor.is_some());
        let second = list(continuation.clone()).await.unwrap();
        assert!(
            matches!(second.items.as_slice(), [DraftRecord::Unconfirmed { draft_id, .. }] if draft_id == "a")
        );
        assert!(second.next_cursor.is_none());
        assert_eq!(
            list(DraftListArgs {
                thread_id: Some("other".into()),
                ..continuation.clone()
            })
            .await
            .unwrap_err()
            .code,
            ErrorCode::CursorExpired
        );
        assert_eq!(
            list(DraftListArgs {
                kind: DraftListKind::Submitted,
                ..continuation
            })
            .await
            .unwrap_err()
            .code,
            ErrorCode::CursorExpired
        );
        for limit in [0, 201] {
            assert_eq!(
                list(DraftListArgs {
                    limit,
                    ..request.clone()
                })
                .await
                .unwrap_err()
                .code,
                ErrorCode::InvalidArgument
            );
        }
        drop(server);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

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
            format!("{}:retention-cancel", owner.operation_epoch.current()),
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
        let snapshot = restore_archive(&control, archive.clone()).await.unwrap();
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
                // The receipt keeps the sent draft's identity after retention.
                assert!(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM receipt WHERE client_operation_id='retention-op'
                       AND json_extract(result_json,'$.draft_submission.source_draft_id')='retention-composer'
                       AND json_extract(result_json,'$.draft_submission.source_revision')=7)",
                    [],
                    |r| r.get::<_, bool>(0)
                )?);
                Ok(())
            })
            .await
            .unwrap();
        let (thread, update) =
            exact_submission(&control, "retention-op", "retention-composer", 7, 42)
                .await
                .unwrap();
        assert_eq!(thread, "thread");
        assert!(
            matches!(update, UiUpdate::ExactSubmission {message_id, generation: 42, ..} if message_id == "retention-message")
        );
        // Simulate a crash-before-ack editable copy and an older unfinished submission.
        owner.db.call(|db| {
            let snapshot=serde_json::to_string(&DraftSnapshot{draft_id:"retention-composer".into(),revision:7,text:"stale unsent copy".into(),..Default::default()}).unwrap();
            db.execute("INSERT INTO draft VALUES('retention-composer','thread',7,?1,'editing','later')",[snapshot])?;
            let snapshot=json!({"draft_id":"retention-composer","revision":6,"text":"older unfinished","attachments":[],"_submission":{"source_draft_id":"retention-composer","source_revision":6}}).to_string();
            db.execute("INSERT INTO draft VALUES('older-submitting','thread',6,?1,'submitting','before')",[snapshot])?;Ok(())
        }).await.unwrap();
        let (updates, incoming) = std::sync::mpsc::sync_channel(128);
        crate::frontend::select_thread(&control, &updates, "thread", 7, 0, None, 80, Vec::new())
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
            restore_archive(&control, "older-submitting".into())
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
            let result = crate::frontend::save_draft(
                &control,
                "thread".into(),
                draft,
                ViewAnchor::default(),
                true,
            )
            .await;
            if revision == 7 {
                assert_eq!(result.unwrap_err().code, ErrorCode::OperationConflict);
            } else {
                result.unwrap();
            }
            let exists=owner.db.call(move|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM draft WHERE id='retention-composer' AND state='editing' AND revision=?1)",[revision as i64],|r|r.get::<_,bool>(0))?)).await.unwrap();
            assert_eq!(exists, revision == 8);
        }
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
