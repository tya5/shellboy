//! Authoritative saved item projection for opt-in history windows.
use crate::protocol::{HistoryExecutionLink, HistoryItem};
use serde_json::Value;

pub const NODE_SOURCE_BYTES: usize = 1024 * 1024;
pub const ITEM_TEXT_BYTES: usize = 16 * 1024;
pub const RESPONSE_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub const ITEM_LINK_LIMIT: usize = 8;

pub fn project(
    node: &str,
    kind: &str,
    payload: &Value,
    links: &[HistoryExecutionLink],
) -> Vec<HistoryItem> {
    use crate::protocol::HistoryItemUnavailableReason as Reason;
    enum Task<'a> {
        Value(&'a Value, String),
    }
    fn identity(node: &str, path: &str) -> String {
        // Coordinates belong to immutable canonical payloads, never to a UI list position.
        serde_json::to_string(&(node, path)).expect("string coordinates serialize")
    }
    fn text(node: &str, path: &str, value: &str) -> HistoryItem {
        let mut end = value.len().min(ITEM_TEXT_BYTES);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        HistoryItem::Text {
            item_identity: identity(node, path),
            text: value[..end].to_owned(),
            text_complete: end == value.len(),
        }
    }
    let mut tasks = Vec::new();
    if kind == "assistant_response" {
        if let Some(output) = payload.get("output").and_then(Value::as_array) {
            for (index, value) in output.iter().enumerate().rev() {
                tasks.push(Task::Value(value, format!("/output/{index}")));
            }
        } else {
            return vec![HistoryItem::Unavailable {
                item_identity: identity(node, ""),
                reason: Reason::Legacy,
            }];
        }
    } else if kind == "tool_result" {
        tasks.push(Task::Value(
            payload.get("item").unwrap_or(payload),
            "/item".into(),
        ));
    } else if payload
        .get("role")
        .and_then(Value::as_str)
        .is_some_and(|role| matches!(role, "user" | "assistant" | "developer" | "system"))
    {
        tasks.push(Task::Value(payload, "".into()));
    } else {
        return vec![HistoryItem::Unavailable {
            item_identity: identity(node, ""),
            reason: Reason::Legacy,
        }];
    }
    let mut result = Vec::new();
    while let Some(Task::Value(value, path)) = tasks.pop() {
        let key = identity(node, &path);
        let tag = value.get("type").and_then(Value::as_str);
        match tag {
            Some("message") | None if tag == Some("message") || value.get("role").is_some() => {
                if let Some(parts) = value.get("content").and_then(Value::as_array) {
                    for (index, part) in parts.iter().enumerate().rev() {
                        tasks.push(Task::Value(part, format!("{path}/content/{index}")));
                    }
                } else if let Some(body) = value.get("content").and_then(Value::as_str) {
                    result.push(text(node, &format!("{path}/content"), body));
                } else {
                    result.push(HistoryItem::Unavailable {
                        item_identity: key,
                        reason: Reason::Unsupported,
                    });
                }
            }
            Some("output_text" | "input_text" | "text" | "refusal") => {
                let body = if tag == Some("refusal") {
                    value.get("refusal")
                } else {
                    value.get("text")
                };
                if let Some(body) = body.and_then(Value::as_str) {
                    result.push(text(node, &path, body));
                } else {
                    result.push(HistoryItem::Unavailable {
                        item_identity: key,
                        reason: Reason::Unsupported,
                    });
                }
            }
            Some("function_call" | "function_call_output") => {
                let mut execution_ids = Vec::new();
                if let Some(call) = value.get("call_id").and_then(Value::as_str) {
                    for link in links.iter().filter(|link| link.call_id == call) {
                        if !execution_ids.contains(&link.execution_id)
                            && execution_ids.len() < ITEM_LINK_LIMIT
                        {
                            execution_ids.push(link.execution_id.clone());
                        }
                    }
                }
                result.push(if tag == Some("function_call") {
                    HistoryItem::ToolCall {
                        item_identity: key,
                        execution_ids,
                    }
                } else {
                    HistoryItem::ToolResult {
                        item_identity: key,
                        execution_ids,
                    }
                });
            }
            Some("reasoning") => {
                result.push(HistoryItem::ReasoningUnavailable { item_identity: key })
            }
            _ => result.push(HistoryItem::Unavailable {
                item_identity: key,
                reason: Reason::Opaque,
            }),
        }
    }
    result
}

pub fn text_bytes(items: &[HistoryItem]) -> usize {
    items
        .iter()
        .map(|item| match item {
            HistoryItem::Text { text, .. } => text.len(),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn mixed_saved_order_and_secrets() {
        let payload = json!({"output":[
            {"type":"message","content":[{"type":"output_text","text":"first"}]},
            {"type":"function_call","call_id":"call","arguments":"SECRET_ARGS"},
            {"type":"reasoning","summary":[{"text":"SECRET_REASON"}],"encrypted_content":"SECRET_OPAQUE"},
            {"type":"message","content":[{"type":"output_text","text":""},{"type":"refusal","refusal":"last"}]}
        ]});
        let items = project("node", "assistant_response", &payload, &[]);
        assert_eq!(items.len(), 5);
        assert!(matches!(&items[0], HistoryItem::Text{text,..} if text=="first"));
        assert!(matches!(&items[1], HistoryItem::ToolCall { .. }));
        assert!(matches!(
            &items[2],
            HistoryItem::ReasoningUnavailable { .. }
        ));
        assert!(matches!(&items[3], HistoryItem::Text{text,..} if text.is_empty()));
        assert!(matches!(&items[4], HistoryItem::Text{text,..} if text=="last"));
        assert!(!serde_json::to_string(&items).unwrap().contains("SECRET"));
    }
    #[test]
    fn utf8_prefix_and_unknown_barrier() {
        let payload = json!({"output":[{"type":"message","content":[{"type":"output_text","text":"界".repeat(6000)}]},{"type":"future","text":"SECRET_UNKNOWN"}]});
        let items = project("node", "assistant_response", &payload, &[]);
        assert!(
            matches!(&items[0], HistoryItem::Text{text,text_complete:false,..} if text.len()<=ITEM_TEXT_BYTES && text.len()%3==0)
        );
        assert!(matches!(&items[1], HistoryItem::Unavailable { .. }));
    }
    #[test]
    fn unknown_role_is_not_prose_and_tool_output_is_not_decoded() {
        let unknown = serde_json::json!({"output":[{"type":"future","role":"assistant","content":[{"type":"output_text","text":"SECRET"}]}]});
        assert!(matches!(
            &project("n", "assistant_response", &unknown, &[])[0],
            HistoryItem::Unavailable { .. }
        ));
        for wrapped in [false, true] {
            let value = serde_json::json!({"type":"function_call_output","call_id":"call","output":"{\"text\":\"SECRET\"}"});
            let payload = if wrapped {
                serde_json::json!({"item":value})
            } else {
                value
            };
            let items = project("n", "tool_result", &payload, &[]);
            assert_eq!(items.len(), 1);
            assert!(matches!(&items[0], HistoryItem::ToolResult { .. }));
            assert!(!serde_json::to_string(&items).unwrap().contains("SECRET"));
        }
        let empty = serde_json::json!({"output":[{"type":"message","content":[]}]});
        assert!(project("n", "assistant_response", &empty, &[]).is_empty());
    }
}
