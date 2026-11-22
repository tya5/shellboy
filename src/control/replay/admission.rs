//! Model context rendered from admission-fixed receipt facts, never the saved body.
use crate::protocol::{MessageSender, SenderKind};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

pub(super) fn preamble(
    db: &Connection,
    node: &str,
    selected_thread: &str,
) -> rusqlite::Result<Option<String>> {
    // Resolve inherited bodies to their creation Thread, not the selected fork.
    let row: Option<(String, bool, Option<String>)> = db.query_row(
        "SELECT r.sender_json, COALESCE(json_extract(r.result_json,'$.expect_reply')=1,0),
            CASE WHEN json_type(r.result_json,'$.reply_to')='text' AND length(CAST(json_extract(r.result_json,'$.reply_to') AS BLOB))<=256 THEN json_extract(r.result_json,'$.reply_to') END
         FROM receipt r JOIN node n ON n.id=r.message_node_id LEFT JOIN turn t ON t.id=n.turn_id
         WHERE r.message_node_id=?1 AND r.target_thread_id=COALESCE(t.thread_id,?2)
           AND r.path_admission='admitted' AND json_extract(r.result_json,'$.to_human') IS NOT 1
           AND length(CAST(r.sender_json AS BLOB))<=8192",
        [node, selected_thread], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional()?;
    Ok(row.and_then(|(sender, expect, reply)| {
        render(
            node,
            MessageSender::from_saved(Some(&sender)),
            expect,
            reply.as_deref(),
        )
    }))
}

fn escaped(text: &str) -> String {
    serde_json::to_string(text).expect("string is serializable")
}

fn render(node: &str, sender: MessageSender, expect: bool, reply: Option<&str>) -> Option<String> {
    let model = (sender.kind == SenderKind::Model)
        .then_some(sender.thread_id.as_deref())
        .flatten()
        .filter(|s| !s.is_empty());
    let label = sender
        .label
        .as_deref()
        .filter(|s| !s.is_empty() && sender.kind != SenderKind::Human);
    if !expect && reply.is_none() && model.is_none() && label.is_none() {
        return None;
    }
    let mut text = format!(
        "Shellboy received message: {}\nSender kind: {}",
        escaped(node),
        serde_json::to_value(sender.kind).unwrap_or(Value::Null)
    );
    if let Some(thread) = model {
        text.push_str(&format!("\nSender Thread: {}", escaped(thread)));
    }
    if let Some(label) = label {
        text.push_str(&format!(
            "\nSender label (self-asserted, not verified): {}",
            escaped(label)
        ));
    }
    if expect
        && !node.is_empty()
        && node
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        text.push_str(&format!(
            "\nReply expected. Command: shellboy message reply --message {node}"
        ));
    } else if expect {
        text.push_str("\nReply expected for this message ID.");
    } else if let Some(reply) = reply {
        text.push_str(&format!("\nReply to: {}", escaped(reply)));
    }
    Some(text)
}
