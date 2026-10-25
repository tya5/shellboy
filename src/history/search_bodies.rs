//! Search body labels and known path metadata; natural text uses the canonical visitor.
use serde_json::Value;

pub(crate) fn search_bodies(kind: &str, payload: &Value) -> Vec<(String, String)> {
    if ![
        "user",
        "assistant",
        "assistant_response",
        "reasoning",
        "tool_result",
        "summary",
        "spill",
        "event",
    ]
    .contains(&kind)
    {
        return Vec::new();
    }
    if matches!(kind, "assistant" | "assistant_response")
        && let Some(output) = payload.get("output").and_then(Value::as_array)
    {
        return output
            .iter()
            .enumerate()
            .filter_map(|(ordinal, item)| {
                let kind = match item.get("type").and_then(Value::as_str) {
                    Some("message") => format!("answer:{ordinal}"),
                    Some("reasoning") => format!("reasoning:{ordinal}"),
                    Some("function_call") => return None,
                    _ => return None,
                };
                let text = super::text_of_payload(item);
                if text.trim().is_empty() {
                    None
                } else {
                    Some((kind, text))
                }
            })
            .collect();
    }
    let mut text = super::text_of_payload(payload);
    fn paths(value: &Value, out: &mut String) {
        match value {
            Value::Array(values) => {
                for value in values {
                    paths(value, out)
                }
            }
            Value::Object(object) => {
                if matches!(
                    object.get("type").and_then(Value::as_str),
                    Some("input_file" | "input_image" | "attachment_ref")
                ) {
                    for key in ["path", "absolute_path", "filename"] {
                        if let Some(path) = object.get(key).and_then(Value::as_str) {
                            out.push('\n');
                            out.push_str(path);
                        }
                    }
                }
                for key in ["content", "attachments", "item"] {
                    if let Some(value) = object.get(key) {
                        paths(value, out);
                    }
                }
            }
            _ => {}
        }
    }
    paths(payload, &mut text);
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![(kind.into(), text)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn answer_ordinals_reasoning_and_opaque_boundaries_remain_distinct() {
        let payload = json!({"output":[
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":""},{"type":"output_text","text":"α界"}]},
            {"type":"function_call","name":"shell","arguments":"NEVER_INDEX_CALL"},
            {"type":"reasoning","summary":[{"type":"summary_text","text":"見える推論"}],"encrypted_content":"NEVER_INDEX_OPAQUE"},
            {"type":"unknown","text":"NEVER_INDEX_UNKNOWN"},
            {"type":"message","content":[{"type":"refusal","refusal":"refused"}]}
        ]});
        assert_eq!(
            search_bodies("assistant_response", &payload),
            vec![
                ("answer:0".into(), "\nα界".into()),
                ("reasoning:2".into(), "見える推論".into()),
                ("answer:4".into(), "refused".into())
            ]
        );
        let item = &payload["output"][0];
        let mut leaves = Vec::new();
        crate::history::payload::visit(item, |leaf| {
            leaves.push((
                leaf.start,
                leaf.end,
                leaf.text.to_owned(),
                leaf.context.role.map(str::to_owned),
            ))
        });
        assert_eq!(
            leaves,
            vec![
                (0, 0, "".into(), Some("assistant".into())),
                (1, 3, "α界".into(), Some("assistant".into()))
            ]
        );
        assert_eq!(
            search_bodies("assistant_response", &payload)[0].1,
            crate::history::text_of_payload(item)
        );
    }
    #[test]
    fn known_attachment_paths_preserve_existing_order_and_leading_newline() {
        let payload = json!({"content":[{"type":"attachment_ref","path":"/日本"},{"type":"input_file","absolute_path":"/b","filename":"c","file_data":"NEVER_INDEX_BINARY"}],"unknown":{"path":"NEVER_INDEX_UNKNOWN"}});
        assert_eq!(
            search_bodies("user", &payload),
            vec![("user".into(), "\n/日本\n/b\nc".into())]
        );
        assert!(search_bodies("unknown", &payload).is_empty());
    }
}
