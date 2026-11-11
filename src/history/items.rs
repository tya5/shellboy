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
                let descriptor = tool_descriptor(value, tag == Some("function_call"));
                let detail_availability = if execution_ids.is_empty() {
                    crate::protocol::ToolDetailAvailability::Unavailable
                } else {
                    crate::protocol::ToolDetailAvailability::ExecutionLinked
                };
                result.push(if tag == Some("function_call") {
                    HistoryItem::ToolCall {
                        item_identity: key,
                        execution_ids,
                        descriptor,
                        detail_availability,
                    }
                } else {
                    HistoryItem::ToolResult {
                        item_identity: key,
                        execution_ids,
                        descriptor,
                        detail_availability,
                    }
                });
            }
            Some("reasoning") => {
                let qualified_parent = kind == "assistant_response"
                    && path.strip_prefix("/output/").is_some_and(|index| {
                        !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                    });
                let before = result.len();
                if qualified_parent {
                    // Complete public qualification table; unknown fields are not traversed.
                    for (field, tag) in [("summary", "summary_text"), ("content", "reasoning_text")]
                    {
                        if let Some(parts) = value.get(field).and_then(Value::as_array) {
                            for (index, part) in parts.iter().enumerate() {
                                if part.get("type").and_then(Value::as_str) != Some(tag) {
                                    continue;
                                }
                                if let Some(readable) = part
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .filter(|text| !text.trim().is_empty())
                                {
                                    let HistoryItem::Text {
                                        item_identity,
                                        text,
                                        text_complete,
                                    } = text(node, &format!("{path}/{field}/{index}"), readable)
                                    else {
                                        unreachable!()
                                    };
                                    result.push(HistoryItem::ReasoningText {
                                        item_identity,
                                        text,
                                        text_complete,
                                    });
                                }
                            }
                        }
                    }
                }
                if before == result.len() {
                    result.push(HistoryItem::ReasoningUnavailable { item_identity: key });
                }
            }
            Some("provider_opaque" | "redacted_thinking") => {
                result.push(HistoryItem::ReasoningUnavailable { item_identity: key });
            }
            _ => result.push(HistoryItem::Unavailable {
                item_identity: key,
                reason: Reason::Opaque,
            }),
        }
    }
    result
}

/// Only the two adopted saved structural fields qualify; no body traversal.
fn tool_descriptor(value: &Value, call: bool) -> crate::protocol::ToolDescriptor {
    use crate::protocol::{ToolDescriptor, ToolFieldAvailability as State};
    fn field(value: Option<&Value>, name: bool) -> (Option<String>, State) {
        let Some(value) = value.filter(|value| !value.is_null()) else { return (None, State::Missing) };
        let Some(text) = value.as_str() else { return (None, State::Invalid) };
        if text.len() > 256 { return (None, State::FieldBudget); }
        if text.is_empty() || (name && (!text.is_ascii() || text.bytes().any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control()))) {
            return (None, State::Invalid);
        }
        (Some(text.to_owned()), State::Available)
    }
    let (call_id, call_id_state) = field(value.get("call_id"), false);
    let (name, name_state) = if call { field(value.get("name"), true) } else { (None, State::NotApplicable) };
    ToolDescriptor { call_id, call_id_state, name, name_state }
}

pub fn text_bytes(items: &[HistoryItem]) -> usize {
    items
        .iter()
        .map(|item| match item {
            HistoryItem::Text { text, .. } | HistoryItem::ReasoningText { text, .. } => text.len(),
            HistoryItem::ToolCall { descriptor, .. } | HistoryItem::ToolResult { descriptor, .. } =>
                descriptor.call_id.as_ref().map_or(0, String::len)
                    + descriptor.name.as_ref().map_or(0, String::len),
            HistoryItem::ReasoningUnavailable { .. }
            | HistoryItem::Unavailable { .. } => 0,
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

#[cfg(test)]
mod readable_reasoning_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn responses_readable_summary() {
        let projected = project(
            "n",
            "assistant_response",
            &json!({"output":[{"type":"reasoning","summary":[{"type":"summary_text","text":" public summary "}]}]}),
            &[],
        );
        let encoded = serde_json::to_value(&projected).unwrap();
        assert_eq!(encoded[0]["kind"], "reasoning_text");
        assert_eq!(encoded[0]["text"], " public summary ");
        assert_eq!(encoded[0]["text_complete"], true);
        assert_eq!(
            encoded[0]["item_identity"],
            r#"["n","/output/0/summary/0"]"#
        );
    }

    #[test]
    fn responses_readable_content() {
        let items = project(
            "n",
            "assistant_response",
            &json!({"output":[{"type":"reasoning","content":[{"type":"reasoning_text","text":"readable content"}]}]}),
            &[],
        );
        let encoded = serde_json::to_value(items).unwrap();
        assert_eq!(encoded[0]["kind"], "reasoning_text");
        assert_eq!(encoded[0]["text"], "readable content");
    }

    #[test]
    fn mixed_readable_ciphertext_never_serialized() {
        let items = project(
            "n",
            "assistant_response",
            &json!({"output":[{"type":"reasoning","encrypted_content":"SECRET_CIPHER","signature":"SECRET_SIGNATURE","summary":[{"type":"summary_text","text":"one"},{"text":"SECRET_UNTYPED"},{"type":"summary_text","text":"two"}],"content":[{"type":"reasoning_text","text":"three"}],"extra":{"text":"SECRET_EXTRA"}}]}),
            &[],
        );
        let encoded = serde_json::to_value(&items).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(
            encoded
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["one", "two", "three"]
        );
        assert!(!serde_json::to_string(&items).unwrap().contains("SECRET"));
    }

    #[test]
    fn encrypted_only_marker() {
        let items = project(
            "n",
            "assistant_response",
            &json!({"output":[{"type":"reasoning","encrypted_content":"SECRET"}]}),
            &[],
        );
        assert!(matches!(
            &items[..],
            [HistoryItem::ReasoningUnavailable { .. }]
        ));
        assert!(!serde_json::to_string(&items).unwrap().contains("SECRET"));
    }

    #[test]
    fn empty_malformed_reasoning_marker() {
        for summary in [
            json!([]),
            json!([{"type":"summary_text","text":""}]),
            json!([{"type":"summary_text","text":" \n\t"}]),
            json!([{"type":"summary_text","text":42}]),
            json!({"type":"summary_text","text":"SECRET"}),
        ] {
            let items = project(
                "n",
                "assistant_response",
                &json!({"output":[{"type":"reasoning","summary":summary}]}),
                &[],
            );
            assert!(matches!(
                &items[..],
                [HistoryItem::ReasoningUnavailable { .. }]
            ));
        }
    }

    #[test]
    fn unknown_reasoning_fields_not_authority() {
        let items = project(
            "n",
            "assistant_response",
            &json!({"output":[{"type":"reasoning","text":"SECRET_TOP","summary":[{"text":"SECRET_NO_TAG"},{"type":"text","text":"SECRET_WRONG_TAG"}],"content":[{"type":"summary_text","text":"SECRET_WRONG_CONTAINER"}]}]}),
            &[],
        );
        assert!(matches!(
            &items[..],
            [HistoryItem::ReasoningUnavailable { .. }]
        ));
        assert!(!serde_json::to_string(&items).unwrap().contains("SECRET"));
    }

    #[test]
    fn opaque_provider_never_interpreted() {
        for kind in ["thinking", "unknown"] {
            let items = project(
                "n",
                "assistant_response",
                &json!({"output":[{"type":"provider_opaque","dialect":"anthropic_messages","kind":kind,"block":{"type":"thinking","thinking":"SECRET_THINKING","signature":"SECRET_SIGNATURE"}}]}),
                &[],
            );
            assert!(matches!(
                &items[..],
                [HistoryItem::ReasoningUnavailable { .. }]
            ));
            assert!(!serde_json::to_string(&items).unwrap().contains("SECRET"));
        }
    }

    #[test]
    fn redacted_reasoning_marker() {
        let items = project(
            "n",
            "assistant_response",
            &json!({"output":[{"type":"redacted_thinking","data":"SECRET_REDACTED"}]}),
            &[],
        );
        assert!(matches!(
            &items[..],
            [HistoryItem::ReasoningUnavailable { .. }]
        ));
    }

    #[test]
    fn reasoning_leaf_requires_exact_saved_responses_parent() {
        let nested = json!({"output":[{"type":"message","content":[{"type":"reasoning","summary":[{"type":"summary_text","text":"SECRET_NESTED"}]}]}]});
        let user = json!({"role":"user","content":[{"type":"reasoning","summary":[{"type":"summary_text","text":"SECRET_USER"}]}]});
        for (kind, payload) in [("assistant_response", nested), ("user", user)] {
            let items = project("n", kind, &payload, &[]);
            assert!(matches!(
                &items[..],
                [HistoryItem::ReasoningUnavailable { .. }]
            ));
            assert!(!serde_json::to_string(&items).unwrap().contains("SECRET"));
        }
    }

    #[test]
    fn ordinary_unknown_unchanged() {
        let items = project(
            "n",
            "assistant_response",
            &json!({"output":[{"type":"future","text":"SECRET"}]}),
            &[],
        );
        assert!(matches!(&items[..], [HistoryItem::Unavailable { .. }]));
    }
}

#[cfg(test)]
mod structural_tool_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn descriptor_both_fields_share_text_byte_ledger() {
        let items = project("node", "assistant_response", &json!({"output":[{"type":"function_call","name":"lookup","call_id":"界".repeat(85)}]}), &[]);
        assert_eq!(text_bytes(&items), 255 + 6);
    }
}
