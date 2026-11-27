//! Native selected-path input recall, independent of the viewport.
use crate::{protocol::*, ui::RecalledPrompt};

pub(super) async fn recall(
    owner: &super::native_history::Owner<'_>,
    thread_id: String,
    before_ordinal: Option<u64>,
    before_node_id: Option<String>,
    older: bool,
) -> Result<Option<RecalledPrompt>, ApiError> {
    let mut args = HistoryRecallArgs {
        thread_id,
        older,
        anchor: before_ordinal
            .zip(before_node_id)
            .map(|(ordinal, node_id)| HistoryRecallAnchor { ordinal, node_id }),
        expected_revision: None,
    };
    // One initial window and one continuation; neither depends on retained size.
    for _ in 0..2 {
        match owner.call(Command::HistoryRecall(args.clone())).await? {
            HistoryRecallResult::Found { input } => {
                return Ok(Some(RecalledPrompt {
                    ordinal: input.ordinal,
                    node_id: input.node_id,
                    text: input.text,
                }));
            }
            HistoryRecallResult::End => return Ok(None),
            HistoryRecallResult::NotInWindow { boundary, revision } => {
                args.anchor = Some(boundary);
                args.expected_revision = Some(revision);
            }
        }
    }
    Err(ApiError::new(
        ErrorCode::InputLimit,
        "No saved input within recall's coverage limit",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn native_recall_continues_once_and_refuses_changed_revision_or_exhausted_coverage() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        owner
            .db
            .call(|db| {
                let tx = db.transaction()?;
                for i in 0..=MAX_LIST_LIMIT {
                    let role = if i == 0 { "user" } else { "assistant" };
                    crate::history::append_node(
                        &tx,
                        "thread",
                        &format!("recall-{i}"),
                        None,
                        role,
                        &json!({"role":role,"content":"preserve exact text\n"}),
                        "2026-11-24T00:00:00Z",
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let native = super::super::selected_owner(&control);
        let args = HistoryRecallArgs {
            thread_id: "thread".into(),
            older: true,
            anchor: None,
            expected_revision: None,
        };
        let read = |args| native.call::<HistoryRecallResult>(Command::HistoryRecall(args));
        let HistoryRecallResult::NotInWindow { boundary, revision } = read(args).await.unwrap()
        else {
            panic!("first window has only assistant nodes")
        };
        assert_eq!(boundary.ordinal, 1);
        let input = recall(&native, "thread".into(), None, None, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (input.ordinal, input.node_id.as_str(), input.text.as_str()),
            (0, "recall-0", "preserve exact text\n")
        );
        owner
            .db
            .call(|db| {
                let tx = db.transaction()?;
                for i in (MAX_LIST_LIMIT + 1)..=(2 * MAX_LIST_LIMIT) {
                    crate::history::append_node(
                        &tx,
                        "thread",
                        &format!("recall-{i}"),
                        None,
                        "assistant",
                        &json!({"role":"assistant","content":"other"}),
                        "2026-11-24T00:00:00Z",
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let stale = read(HistoryRecallArgs {
            thread_id: "thread".into(),
            older: true,
            anchor: Some(boundary),
            expected_revision: Some(revision),
        })
        .await
        .unwrap_err();
        assert_eq!(stale.code, ErrorCode::CursorExpired);
        let limit = recall(&native, "thread".into(), None, None, true)
            .await
            .unwrap_err();
        assert_eq!(limit.code, ErrorCode::InputLimit);
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
