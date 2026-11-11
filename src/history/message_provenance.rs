//! Fail-closed saved-fact validation; emission additionally requires response budgeting.
use crate::protocol::{MessageAccepted, MessageProvenance, MessageReplyState};
use serde_json::Value;

pub(super) const REVERSE_LOOKUP: &str = "SELECT id,message_node_id,result_json FROM receipt INDEXED BY receipt_reply_node_lookup WHERE target_thread_id=?1 AND (CASE WHEN json_valid(result_json) THEN CASE WHEN json_type(result_json,'$.reply.node_id')='text' THEN json_extract(result_json,'$.reply.node_id') END END)=?2 LIMIT 2";

fn id(value: &str, source: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && value != source
}

pub(super) fn saved_relation(raw: &str, body: &str, thread: &str) -> Option<MessageProvenance> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let accepted: MessageAccepted = serde_json::from_value(value.clone()).ok()?;
    if accepted.message_id != body || accepted.thread_id != thread { return None; }
    let mut relation = MessageProvenance {
        expect_reply: accepted.expect_reply, reply_to: None, reply_state: None, reply_node_id: None,
    };
    if let Some(reply_to) = value.get("reply_to") {
        let target = reply_to.as_str()?;
        if accepted.expect_reply || value.get("reply").is_some() || !id(target, body) { return None; }
        relation.reply_to = Some(target.into());
    } else if accepted.expect_reply {
        let reply = value.get("reply")?.as_object()?;
        let state = reply.get("state")?.as_str()?;
        relation.reply_state = Some(match state {
            "pending" => MessageReplyState::Pending,
            "replied" => MessageReplyState::Replied,
            "ignored" => MessageReplyState::Ignored,
            "cancelled" => MessageReplyState::Cancelled,
            _ => return None,
        });
        if state == "replied" {
            let target = reply.get("node_id")?.as_str()?;
            if !id(target, body) { return None; }
            relation.reply_node_id = Some(target.into());
        } else if reply.contains_key("node_id") { return None; }
    } else if value.get("reply").is_some() { return None; }
    (serde_json::to_vec(&relation).ok()?.len() <= 4096).then_some(relation)
}
