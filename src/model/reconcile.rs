//! Classify received differences for diagnostics only; never decide adoption.
use super::OutputMismatchField as Field;
use serde_json::Value;

pub(super) fn mismatch(done: &Value, final_item: &Value) -> Option<Field> {
    if done.get("type") != final_item.get("type") {
        return Some(Field::Type);
    }
    match done.get("type").and_then(Value::as_str) {
        Some("message") => {
            if done.get("role") != final_item.get("role") {
                return Some(Field::Role);
            }
            text_parts(done.get("content"), final_item.get("content"))
        }
        Some("function_call") => {
            for (key, field) in [("call_id", Field::CallId), ("name", Field::Name)] {
                if done.get(key) != final_item.get(key) {
                    return Some(field);
                }
            }
            let left = done.get("arguments").and_then(Value::as_str);
            let right = final_item.get("arguments").and_then(Value::as_str);
            if left == right {
                return None;
            }
            // JSON formatting is not a command change. Invalid arguments still
            // go through the existing all-calls validation before execution.
            let equal_json = left.zip(right).is_some_and(|(left, right)| {
                if left.len() > super::MAX_TOOL_ARGUMENT_BYTES
                    || right.len() > super::MAX_TOOL_ARGUMENT_BYTES
                {
                    return false;
                }
                match (
                    serde_json::from_str::<super::ShellToolArgs>(left),
                    serde_json::from_str::<super::ShellToolArgs>(right),
                ) {
                    (Ok(left), Ok(right)) => left == right,
                    _ => false,
                }
            });
            (!equal_json).then_some(Field::Arguments)
        }
        Some("reasoning") => {
            // Describe differences in received opaque content and summary text.
            // The validated final response remains the only adoption source.
            for (key, field) in [
                ("summary", Field::ReasoningSummary),
                ("content", Field::ReasoningContent),
            ] {
                if let Some(left) = done.get(key).filter(|value| !value.is_null())
                    && text_parts(Some(left), final_item.get(key)).is_some()
                {
                    return Some(field);
                }
            }
            if let Some(encrypted) = done
                .get("encrypted_content")
                .filter(|value| !value.is_null())
                && Some(encrypted) != final_item.get("encrypted_content")
            {
                return Some(Field::EncryptedContent);
            }
            None
        }
        // ModelResponse::completed rejects unsupported items with the original
        // final JSON retained. Do not invent semantics for an unknown item.
        _ => None,
    }
}

fn text_parts(left: Option<&Value>, right: Option<&Value>) -> Option<Field> {
    let (Some(left), Some(right)) = (
        left.and_then(Value::as_array),
        right.and_then(Value::as_array),
    ) else {
        return Some(Field::Content);
    };
    if left.len() != right.len() {
        return Some(Field::Content);
    }
    for (left, right) in left.iter().zip(right) {
        if left.get("type") != right.get("type") {
            return Some(Field::Content);
        }
        let (key, field) = match left.get("type").and_then(Value::as_str) {
            Some("output_text" | "summary_text" | "reasoning_text") => ("text", Field::Text),
            Some("refusal") => ("refusal", Field::Refusal),
            _ => return Some(Field::Content),
        };
        if left.get(key) != right.get(key) {
            return Some(field);
        }
    }
    None
}

/// Delta events do not necessarily identify an output item. Compare only
/// received, nonempty channels against the validated final response, for audit.
pub(super) fn delta_differences(
    text: &str,
    reasoning: &str,
    response: &super::ModelResponse,
) -> Vec<Field> {
    let mut differences = Vec::with_capacity(2);
    if !text.is_empty() && text != response.text {
        differences.push(Field::Text);
    }
    if !reasoning.is_empty() {
        let final_reasoning = response
            .output
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
            .flat_map(|item| {
                ["summary", "content"].into_iter().flat_map(move |key| {
                    item.get(key)
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
            })
            .filter(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("summary_text" | "reasoning_text")
                )
            })
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .flat_map(str::chars);
        if !final_reasoning.eq(reasoning.chars()) {
            differences.push(Field::ReasoningContent);
        }
    }
    differences
}
