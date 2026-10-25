//! Borrowed natural-language leaves in canonical history order.
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum LeafKind {
    #[default]
    Natural,
    Reasoning,
    ToolCall,
    ToolResult,
}
#[derive(Clone, Copy, Default)]
pub(crate) struct Context<'a> {
    pub kind: LeafKind,
    pub reasoning: bool,
    pub role: Option<&'a str>,
    pub call: Option<&'a Value>,
    pub call_ordinal: Option<u32>,
}
pub(crate) struct Leaf<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
    pub context: Context<'a>,
}
/// Visit known text fields without cloning payloads or using recursive Rust frames.
/// Opaque/unknown types are excluded. Empty leaves still own their newline separator.
pub(crate) fn visit(payload: &Value, mut leaf: impl FnMut(Leaf<'_>)) {
    enum Task<'a> {
        Value(&'a Value, Context<'a>),
        Text(&'a str, Context<'a>),
    }
    let mut stack = vec![Task::Value(payload, Context::default())];
    let mut position = 0;
    let mut seen = false;
    let mut call_ordinal = 0u32;
    while let Some(task) = stack.pop() {
        match task {
            Task::Text(text, context) => {
                position += usize::from(seen);
                seen = true;
                let start = position;
                position += text.chars().count();
                leaf(Leaf {
                    text,
                    start,
                    end: position,
                    context,
                });
            }
            Task::Value(value, context) => match value {
                Value::String(text) => stack.push(Task::Text(text, context)),
                Value::Array(values) => {
                    stack.extend(values.iter().rev().map(|value| Task::Value(value, context)));
                }
                Value::Object(object) => {
                    let kind = object.get("type").and_then(Value::as_str);
                    if kind.is_some_and(|kind| {
                        !matches!(
                            kind,
                            "message"
                                | "input_text"
                                | "output_text"
                                | "text"
                                | "reasoning"
                                | "reasoning_text"
                                | "summary_text"
                                | "function_call"
                                | "function_call_output"
                                | "refusal"
                                | "summary"
                                | "spill"
                        )
                    }) {
                        continue;
                    }
                    let mut context = context;
                    if let Some(role) = object.get("role").and_then(Value::as_str) {
                        context.role = Some(role);
                    }
                    match kind {
                        Some("reasoning" | "reasoning_text" | "summary_text") => {
                            context.kind = LeafKind::Reasoning;
                            context.reasoning = true;
                        }
                        Some("function_call") => {
                            context.call_ordinal = Some(call_ordinal);
                            call_ordinal = call_ordinal.saturating_add(1);
                            context.kind = LeafKind::ToolCall;
                            context.call = Some(value);
                        }
                        Some("function_call_output") => {
                            context.call_ordinal = None;
                            context.kind = LeafKind::ToolResult;
                            context.call = Some(value);
                        }
                        _ => {}
                    }
                    // Push reverse canonical order: refusal, text, containers, arguments.
                    if kind == Some("function_call")
                        && let Some(arguments) = object.get("arguments").and_then(Value::as_str)
                    {
                        stack.push(Task::Text(arguments, context));
                    }
                    for key in ["response", "item", "summary", "output", "content", "text"] {
                        if let Some(value) = object.get(key) {
                            stack.push(Task::Value(value, context));
                        }
                    }
                    if let Some(refusal) = object.get("refusal").and_then(Value::as_str) {
                        stack.push(Task::Text(refusal, context));
                    }
                }
                _ => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_order_offsets_and_inherited_context_include_empty_leaves() {
        let payload = json!({"refusal":"拒否", "text":"", "content":[
            {"type":"reasoning", "summary":[{"type":"summary_text", "text":"考🦀"}], "encrypted_content":"opaque"},
            {"type":"vendor_extension", "text":"hidden"},
            {"type":"function_call", "call_id":"a", "arguments":"{\"command\":\"echo a\"}"},
            {"type":"function_call_output", "call_id":"a", "output":"ok"}
        ], "response":{"text":"末尾"}});
        let mut leaves = Vec::new();
        visit(&payload, |leaf| {
            leaves.push((
                leaf.text.to_owned(),
                leaf.start,
                leaf.end,
                leaf.context.kind,
                leaf.context.reasoning,
                leaf.context
                    .call
                    .and_then(|call| call.get("call_id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ))
        });
        assert_eq!(
            leaves
                .iter()
                .map(|leaf| leaf.0.as_str())
                .collect::<Vec<_>>(),
            ["拒否", "", "考🦀", "{\"command\":\"echo a\"}", "ok", "末尾"]
        );
        assert_eq!(
            (leaves[2].1, leaves[2].2, leaves[2].3, leaves[2].4),
            (4, 6, LeafKind::Reasoning, true)
        );
        assert_eq!(
            (leaves[3].3, leaves[3].5.as_deref()),
            (LeafKind::ToolCall, Some("a"))
        );
        assert_eq!(
            (leaves[4].3, leaves[4].5.as_deref()),
            (LeafKind::ToolResult, Some("a"))
        );
    }

    #[test]
    fn deeply_nested_known_containers_keep_canonical_text_without_recursive_walking() {
        let mut payload = json!({"type":"output_text", "text":"深い本文"});
        for _ in 0..256 {
            payload = json!({"content":[payload]});
        }
        let mut found = Vec::new();
        visit(&payload, |leaf| {
            found.push((leaf.text.to_owned(), leaf.start, leaf.end))
        });
        assert_eq!(found, [("深い本文".to_owned(), 0, 4)]);
    }
}
