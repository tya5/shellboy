//! Small provider facts, independent of usage counters and routing decisions.
use serde_json::{Value, json};

pub(crate) const MAX_DIAGNOSTIC_BYTES: usize = 2048;
pub(crate) const MAX_RESPONSE_ID_BYTES: usize = 1024;

pub(crate) fn response_id(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.is_empty() && s.len() <= MAX_RESPONSE_ID_BYTES)
}

pub(crate) fn bounded(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let kind = object.get("type")?.as_str()?;
    if kind.is_empty() || kind.len() > 64 {
        return None;
    }
    let mut result = json!({"type":kind});
    if let Some(reason) = object.get("reason").and_then(Value::as_str).filter(|s| s.len() <= 256) {
        result["reason"] = json!(reason);
    }
    if let Some(tokens) = object.get("cache_missed_tokens").and_then(Value::as_i64).filter(|n| *n >= 0) {
        result["cache_missed_tokens"] = json!(tokens);
    }
    debug_assert!(result.to_string().len() <= MAX_DIAGNOSTIC_BYTES);
    Some(result)
}

pub(crate) fn normalize_response(response: &mut Value) {
    if let Some(object) = response.as_object_mut()
        && let Some(value) = object.remove("prompt_cache_diagnostics")
        && let Some(value) = bounded(&value)
    {
        object.insert("prompt_cache_diagnostics".into(), value);
    }
}
