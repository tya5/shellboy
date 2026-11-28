//! Selected-page queue facts from native reads: the window observation's
//! pending rows, the Thread's human duties, known draft submissions and
//! locally tracked receipts. No private database reader is used.
use super::native_history::Owner;
use crate::protocol::{
    ApiError, Command, DeliveryStatus, ErrorCode, HistoryWindowPending, MessageDetails,
    MessageListArgs, MessageTarget, OperationReceiptDetails, OperationTarget, TurnDetails,
    TurnState, TurnTarget,
};
use crate::ui::{PendingMessage, PendingMessages, ReceiptStatus, ReplyTarget, SubmittedMessage};
use serde_json::Value;

/// A draft operation this client submitted to a Thread whose saved outcome
/// it has not received. The frontend loop keeps at most this many per Thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KnownSubmission {
    pub operation_id: String,
    pub draft_id: String,
    pub revision: u64,
}
pub(super) const KNOWN_SUBMISSIONS: usize = 128;

/// Remember one submission, keeping only the newest bounded set.
pub(super) fn remember(known: &mut Vec<KnownSubmission>, submission: KnownSubmission) {
    known.retain(|old| old.operation_id != submission.operation_id);
    known.push(submission);
    if known.len() > KNOWN_SUBMISSIONS {
        known.remove(0);
    }
}

/// The queue the selected page installs: pending membership and holds come
/// from the window observation; duties and submissions are separate reads.
pub(super) async fn queue(
    owner: &Owner<'_>,
    thread: &str,
    pending: &HistoryWindowPending,
    known: &[KnownSubmission],
) -> Result<PendingMessages, ApiError> {
    Ok(PendingMessages {
        items: pending.items.iter().map(pending_message).collect(),
        total: pending.total,
        submitted: submitted(owner, known).await?,
        awaiting_replies: human_duties(owner, thread).await?,
    })
}

fn pending_message(row: &crate::protocol::HistoryWindowPendingMessage) -> PendingMessage {
    PendingMessage {
        held_by: row.held_by.clone(),
        message_id: row.message_id.clone(),
        operation_id: row.operation_id.clone(),
        sender_thread: row.sender.thread_id.clone(),
        sender_kind: row.sender.kind,
        completion_scheduled: row.completion_scheduled,
        created_at: row.created_at.clone(),
        held: row.delivery_status == DeliveryStatus::Held,
        uncertain: row.admission_uncertain,
        excerpt: row.excerpt.clone(),
        excerpt_truncated: row.excerpt_truncated,
        attachment_count: row.attachment_count,
        expect_reply: row.expect_reply,
    }
}

/// This Thread's open questions to a human. A named ask is saved as applied,
/// so every listed duty can be answered.
async fn human_duties(owner: &Owner<'_>, thread: &str) -> Result<Vec<ReplyTarget>, ApiError> {
    let page: crate::protocol::RecordPage<Value, Value> = owner
        .call(Command::MessageList(MessageListArgs {
            owed_by_thread_id: None,
            asked_by_thread_id: Some(thread.to_owned()),
            to: Some("human".into()),
            limit: crate::protocol::MAX_PENDING_REPLY_DUTIES as u32,
            cursor: None,
        }))
        .await?;
    let now = chrono::Utc::now();
    page.items
        .iter()
        .map(|item| {
            let text = |key: &str| item.get(key).and_then(Value::as_str);
            let (Some(message_id), Some(created_at)) = (text("message_id"), text("created_at"))
            else {
                return Err(invalid());
            };
            let created =
                chrono::DateTime::parse_from_rfc3339(created_at).map_err(|_| invalid())?;
            Ok(ReplyTarget {
                message_id: message_id.to_owned(),
                sender: "Human decision".into(),
                created_at: created_at.chars().take(128).collect(),
                applied: true,
                to_human: true,
                recipient_thread_id: thread.to_owned(),
                elapsed_ms: now.signed_duration_since(created).num_milliseconds().max(0) as u64,
                excerpt: crate::presentation::sanitize_plain(text("excerpt").unwrap_or(""), 0)
                    .0
                    .chars()
                    .take(512)
                    .collect(),
            })
        })
        .collect()
}

/// Settle each known submission from its exact operation receipt.
async fn submitted(
    owner: &Owner<'_>,
    known: &[KnownSubmission],
) -> Result<Vec<SubmittedMessage>, ApiError> {
    let mut settled = Vec::new();
    for submission in known.iter().take(KNOWN_SUBMISSIONS) {
        let receipt: OperationReceiptDetails = match owner
            .call(Command::OperationGet(OperationTarget {
                operation_id: submission.operation_id.clone(),
            }))
            .await
        {
            Ok(receipt) => receipt,
            Err(error) if error.code == ErrorCode::OperationNotFound => continue,
            Err(error) => return Err(error),
        };
        if receipt.operation_id != submission.operation_id {
            return Err(invalid());
        }
        if let Some(message_id) = receipt.targets.get("message_id") {
            settled.push(SubmittedMessage {
                operation_id: submission.operation_id.clone(),
                source_draft_id: submission.draft_id.clone(),
                revision: submission.revision,
                message_id: message_id.clone(),
            });
        }
    }
    Ok(settled)
}

/// Saved delivery state of locally tracked messages. A missing message has no
/// state to deliver; an applied one is completed only when its Turn is.
pub(super) async fn receipts(
    owner: &Owner<'_>,
    ids: &[String],
) -> Result<Vec<(String, ReceiptStatus)>, ApiError> {
    let mut states = Vec::new();
    for id in ids {
        let message: MessageDetails = match owner
            .call(Command::MessageGet(MessageTarget {
                message_id: id.clone(),
            }))
            .await
        {
            Ok(message) => message,
            Err(error) if error.code == ErrorCode::TargetUnavailable => continue,
            Err(error) => return Err(error),
        };
        if message.message_id != *id {
            return Err(invalid());
        }
        let turn = match (message.delivery_status, &message.turn_id) {
            (DeliveryStatus::Applied, Some(turn)) => {
                let details: TurnDetails = owner
                    .call(Command::TurnGet(TurnTarget {
                        turn_id: turn.clone(),
                    }))
                    .await?;
                Some(details.state)
            }
            _ => None,
        };
        let receipt = match message.delivery_status {
            DeliveryStatus::Accepted => "accepted",
            DeliveryStatus::Held => "held",
            DeliveryStatus::Applied => "applied",
            DeliveryStatus::Cancelled => "cancelled",
        };
        let turn = turn.map(|state| match state {
            TurnState::Completed => "completed",
            _ => "other",
        });
        if let Some(status) = super::receipt_delivery::project_state(receipt, turn) {
            states.push((id.clone(), status));
        }
    }
    Ok(states)
}

fn invalid() -> ApiError {
    ApiError::new(
        ErrorCode::ProtocolMismatch,
        "Invalid selected queue response",
    )
}

/// The selected Thread's installed queue, read as one tail install would.
#[cfg(test)]
pub(super) async fn read_queue(
    control: &crate::control::Control,
    thread: &str,
) -> Result<PendingMessages, ApiError> {
    let owner = Owner {
        data_dir: super::compose_data_dir(control),
        instance: &control.store.instance_id,
    };
    let window = owner
        .window(super::history_reader::window_args(
            thread,
            super::history_reader::Anchor::Tail,
            0,
            0,
            Vec::new(),
        ))
        .await?;
    queue(
        &owner,
        thread,
        &window.observation.expect("observed").pending,
        &[],
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The installed queue: observed pending rows, this Thread's human duties,
    /// known submissions settled by exact receipt, and tracked receipts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_queue_reads_rows_duties_submissions_and_receipts() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let instance = control.store.instance_id.clone();
        owner.db.call(move |db| {
            let tx = db.transaction()?;
            let message = |text: &str| json!({"role":"user","content":[{"type":"input_text","text":text}]}).to_string();
            for (id, text) in [("held", "held input"), ("question", "choose A or B"), ("applied", "done")] {
                tx.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','2026-01-01T00:00:00Z',?2)", rusqlite::params![id, message(text)])?;
            }
            tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,updated_at,path_admission,hold_reason,result_json)VALUES('held','held-op','{\"kind\":\"model\",\"instance_id\":\"i\",\"thread_id\":\"independent\",\"run_id\":\"r\",\"turn_id\":\"u\",\"execution_id\":\"e\"}','thread','held','held','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','unknown','uncertain_path_admission','{\"deliver\":\"complete\",\"delivery_context\":{\"reason\":\"run_completion\"},\"reply\":{\"state\":\"pending\"}}')", [])?;
            tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,updated_at,path_admission,result_json)VALUES('question','question-op','{\"kind\":\"model\",\"thread_id\":\"thread\"}','thread','question','applied','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','admitted','{\"to_human\":true,\"ask_name\":\"human\",\"reply\":{\"state\":\"pending\"}}')", [])?;
            tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,updated_at,path_admission)VALUES('applied','applied-op','null','thread','applied','target-run','target-run','applied','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','admitted')", [])?;
            tx.execute("UPDATE turn SET state='completed' WHERE id='target-run'", [])?;
            tx.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes)VALUES('submit-op',?2,'null','hash','completed',?3,?1,0)", rusqlite::params![json!({"result":{"message_id":"applied","thread_id":"thread"}}).to_string(), instance, chrono::Utc::now().to_rfc3339()])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let owner_read = Owner {
            data_dir: super::super::compose_data_dir(&control),
            instance: &control.store.instance_id,
        };
        let window = owner_read
            .window(super::super::history_reader::window_args(
                "thread",
                super::super::history_reader::Anchor::Tail,
                0,
                0,
                Vec::new(),
            ))
            .await
            .unwrap();
        let known = [
            KnownSubmission {
                operation_id: "submit-op".into(),
                draft_id: "draft".into(),
                revision: 3,
            },
            KnownSubmission {
                operation_id: "never-saved".into(),
                draft_id: "draft".into(),
                revision: 4,
            },
        ];
        let queue = queue(
            &owner_read,
            "thread",
            &window.observation.unwrap().pending,
            &known,
        )
        .await
        .unwrap();
        assert_eq!(queue.total, 1);
        let row = &queue.items[0];
        assert_eq!(row.message_id, "held");
        assert_eq!(row.sender_thread.as_deref(), Some("independent"));
        assert!(row.held && row.uncertain && row.expect_reply && row.completion_scheduled);
        assert_eq!(
            row.held_by,
            Some(crate::protocol::MessageHeldBy::UncertainPathAdmission)
        );
        assert_eq!(queue.awaiting_replies.len(), 1);
        let duty = &queue.awaiting_replies[0];
        assert_eq!(duty.message_id, "question");
        assert!(duty.applied && duty.to_human);
        assert_eq!(duty.excerpt, "choose A or B");
        assert_eq!(
            queue.submitted,
            [SubmittedMessage {
                operation_id: "submit-op".into(),
                source_draft_id: "draft".into(),
                revision: 3,
                message_id: "applied".into(),
            }]
        );
        let states = receipts(
            &owner_read,
            &["held".into(), "applied".into(), "absent".into()],
        )
        .await
        .unwrap();
        assert_eq!(
            states,
            [
                ("held".to_owned(), ReceiptStatus::Held),
                ("applied".to_owned(), ReceiptStatus::Completed)
            ]
        );
        let mut tracked = Vec::new();
        for revision in 0..(KNOWN_SUBMISSIONS as u64 + 2) {
            remember(
                &mut tracked,
                KnownSubmission {
                    operation_id: format!("op-{revision}"),
                    draft_id: "draft".into(),
                    revision,
                },
            );
        }
        assert_eq!(tracked.len(), KNOWN_SUBMISSIONS);
        assert_eq!(
            tracked[0].operation_id, "op-2",
            "the newest bounded set is kept"
        );
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
