//! Saved input source facts for presentation, from the owner's node
//! provenance; never inferred from rendered roles.
use super::native_history::Owner;
use crate::{
    presentation::{InputSource, ReplyState},
    protocol::{
        ApiError, Command, ErrorCode, HistoryBody, HistoryReadArgs, MessageDeliveryReason,
        MessageReplyState, NodeProvenance, SenderKind, ThreadDetails, ThreadGetArgs,
    },
    ui::UiNode,
};
use std::collections::HashMap;

/// Characters of a replied-to request the row shows.
const REPLY_EXCERPT_CHARS: usize = 512;
// A UTF-8 character is at most four bytes; one more character proves a cut.
const REPLY_EXCERPT_BYTES: u32 = 4 * (REPLY_EXCERPT_CHARS as u32 + 1);

/// What classifying a page's provenance needs beyond the provenance itself.
pub(super) struct Sources {
    viewed: String,
    parent: Option<String>,
    excerpts: HashMap<String, String>,
}

/// Read the viewed Thread's parent only when a model sender from another
/// Thread needs it (`parent` is `None` when the caller has not read it), and
/// the opening text of each replied-to request, from the owner.
pub(super) async fn read<'p>(
    owner: &Owner<'_>,
    viewed: &str,
    parent: Option<Option<String>>,
    provenance: impl IntoIterator<Item = &'p NodeProvenance>,
) -> Result<Sources, ApiError> {
    let mut foreign_model = false;
    let mut replied = Vec::new();
    for provenance in provenance {
        if provenance.sender.as_ref().is_some_and(|sender| {
            sender.kind == SenderKind::Model && sender.thread_id.as_deref() != Some(viewed)
        }) {
            foreign_model = true;
        }
        if let Some(target) = provenance
            .message
            .as_ref()
            .and_then(|message| message.reply_to.as_deref())
            && !replied.iter().any(|known| known == target)
        {
            replied.push(target.to_owned());
        }
    }
    let parent = match parent {
        Some(parent) => parent,
        None if foreign_model => {
            let details: ThreadDetails = owner
                .call(Command::ThreadGet(ThreadGetArgs {
                    thread_id: Some(viewed.to_owned()),
                    display: false,
                }))
                .await?;
            if details.thread_id != viewed {
                return Err(ApiError::new(
                    ErrorCode::ProtocolMismatch,
                    "Thread read did not match the viewed Thread",
                ));
            }
            details.parent_thread_id
        }
        None => None,
    };
    let mut excerpts = HashMap::with_capacity(replied.len());
    for target in replied {
        let body: Result<HistoryBody, ApiError> = owner
            .call(Command::HistoryRead(HistoryReadArgs {
                display: None,
                end_message_only: false,
                parts: false,
                node_id: target.clone(),
                limit_bytes: REPLY_EXCERPT_BYTES,
                cursor: None,
                encoding: "UTF-8".into(),
            }))
            .await;
        match body {
            Ok(body) if body.node_id == target && body.tombstone_reason.is_none() => {
                excerpts.insert(target, excerpt(&body.text));
            }
            Ok(body) if body.node_id == target => {}
            Ok(_) => {
                return Err(ApiError::new(
                    ErrorCode::ProtocolMismatch,
                    "History read did not match the replied-to request",
                ));
            }
            // The replied-to request is gone: the row names only the reply.
            Err(error) if error.code == ErrorCode::TargetUnavailable => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Sources {
        viewed: viewed.to_owned(),
        parent,
        excerpts,
    })
}

fn excerpt(text: &str) -> String {
    crate::presentation::sanitize_plain(text, 0)
        .0
        .replace(['\n', '\r'], " ")
        .chars()
        .take(REPLY_EXCERPT_CHARS)
        .collect()
}

/// Project one node's saved provenance into its presentation.
pub(super) fn apply(node: &mut UiNode, provenance: &NodeProvenance, sources: &Sources) {
    let presentation = &mut node.presentation;
    presentation.created_at = Some(provenance.created_at.clone()).filter(|at| at.len() <= 128);
    presentation.input_source = source(provenance, sources);
    presentation.input_sender_label = provenance
        .sender
        .as_ref()
        .and_then(|sender| sender.label.clone())
        .filter(|label| !label.trim().is_empty());
    let message = provenance.message.as_ref();
    presentation.reply_state = message
        .and_then(|message| message.reply_state)
        .map(|state| match state {
            MessageReplyState::Pending => ReplyState::Pending,
            MessageReplyState::Replied => ReplyState::Replied,
            MessageReplyState::Ignored => ReplyState::Ignored,
            MessageReplyState::Cancelled => ReplyState::Cancelled,
        });
    presentation.reply_excerpt = message
        .and_then(|message| message.reply_to.as_deref())
        .and_then(|target| sources.excerpts.get(target).cloned());
    presentation.bound_metadata();
}

fn source(provenance: &NodeProvenance, sources: &Sources) -> InputSource {
    if provenance.delivery_reason == Some(MessageDeliveryReason::RunCompletion) {
        return InputSource::Revisit;
    }
    let Some(sender) = &provenance.sender else {
        return InputSource::Unknown;
    };
    match sender.kind {
        SenderKind::Human => InputSource::Human,
        SenderKind::Model => match sender.thread_id.as_deref() {
            Some(thread) if thread == sources.viewed => InputSource::SelfModel,
            Some(thread) if Some(thread) == sources.parent.as_deref() => InputSource::Parent,
            _ => InputSource::OtherModel,
        },
        SenderKind::ExternalCli | SenderKind::Unknown => InputSource::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{MessageProvenance, MessageSender};

    fn provenance(sender: Option<MessageSender>) -> NodeProvenance {
        NodeProvenance {
            output_limit: None,
            notification: None,
            message: None,
            sender,
            delivery_reason: None,
            client_operation_id: None,
            attempt_id: None,
            kind: "user".into(),
            created_at: "2026-11-01T09:30:00Z".into(),
            source_revision: 0,
            turn_id: None,
            run_id: None,
            origin_thread_id: Some("child".into()),
        }
    }
    fn model(thread: &str) -> MessageSender {
        MessageSender {
            kind: SenderKind::Model,
            thread_id: Some(thread.into()),
            ..Default::default()
        }
    }
    fn sources(parent: Option<&str>) -> Sources {
        Sources {
            viewed: "child".into(),
            parent: parent.map(str::to_owned),
            excerpts: HashMap::from([("asked".into(), "source request".into())]),
        }
    }

    #[test]
    fn input_source_follows_saved_sender_and_delivery_reason_relative_to_the_view() {
        let human = MessageSender {
            kind: SenderKind::Human,
            ..Default::default()
        };
        let labelled = MessageSender {
            kind: SenderKind::ExternalCli,
            label: Some("human".into()),
            ..Default::default()
        };
        let mut revisit = provenance(Some(human.clone()));
        revisit.delivery_reason = Some(MessageDeliveryReason::RunCompletion);
        let mut carried = provenance(Some(human.clone()));
        carried.delivery_reason = Some(MessageDeliveryReason::CarriedAfterTurnInterruption);
        for (provenance, parent, expected) in [
            (provenance(Some(human)), Some("parent"), InputSource::Human),
            (revisit, Some("parent"), InputSource::Revisit),
            (carried, Some("parent"), InputSource::Human),
            (
                provenance(Some(model("child"))),
                None,
                InputSource::SelfModel,
            ),
            (
                provenance(Some(model("parent"))),
                Some("parent"),
                InputSource::Parent,
            ),
            (
                provenance(Some(model("parent"))),
                None,
                InputSource::OtherModel,
            ),
            (
                provenance(Some(model("elsewhere"))),
                Some("parent"),
                InputSource::OtherModel,
            ),
            (provenance(Some(labelled)), None, InputSource::Unknown),
            (provenance(None), None, InputSource::Unknown),
        ] {
            let mut node = UiNode::default();
            apply(&mut node, &provenance, &sources(parent));
            assert_eq!(node.presentation.input_source, expected, "{provenance:?}");
            assert_eq!(
                node.presentation.created_at.as_deref(),
                Some("2026-11-01T09:30:00Z")
            );
        }
    }

    #[test]
    fn a_self_asserted_label_is_shown_but_never_changes_sender_authority() {
        let mut node = UiNode::default();
        let saved = provenance(Some(MessageSender {
            kind: SenderKind::ExternalCli,
            label: Some("human".into()),
            ..Default::default()
        }));
        apply(&mut node, &saved, &sources(None));
        assert_eq!(node.presentation.input_source, InputSource::Unknown);
        assert_eq!(
            node.presentation.input_sender_label.as_deref(),
            Some("human")
        );
    }

    #[test]
    fn reply_rows_show_the_saved_state_and_the_replied_to_request() {
        let mut asked = provenance(Some(model("parent")));
        asked.message = Some(MessageProvenance {
            expect_reply: true,
            reply_to: None,
            reply_state: Some(MessageReplyState::Pending),
            reply_node_id: None,
        });
        let mut reply = provenance(Some(model("parent")));
        reply.message = Some(MessageProvenance {
            expect_reply: false,
            reply_to: Some("asked".into()),
            reply_state: None,
            reply_node_id: None,
        });
        let mut nodes = [UiNode::default(), UiNode::default()];
        apply(&mut nodes[0], &asked, &sources(Some("parent")));
        apply(&mut nodes[1], &reply, &sources(Some("parent")));
        assert_eq!(nodes[0].presentation.reply_state, Some(ReplyState::Pending));
        assert!(nodes[0].presentation.reply_excerpt.is_none());
        assert_eq!(
            nodes[1].presentation.reply_excerpt.as_deref(),
            Some("source request")
        );
        assert_eq!(nodes[1].presentation.input_source, InputSource::Parent);
    }

    #[test]
    fn reply_excerpts_are_one_bounded_plain_line() {
        let text = format!("first\nsecond\x1b[2J{}", "界".repeat(600));
        let excerpt = excerpt(&text);
        assert!(!excerpt.contains(['\n', '\r', '\x1b']));
        assert_eq!(excerpt.chars().count(), REPLY_EXCERPT_CHARS);
    }
}
