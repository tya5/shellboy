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
    if let Some(reason) = object
        .get("reason")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 256)
    {
        result["reason"] = json!(reason);
    }
    if let Some(tokens) = object
        .get("cache_missed_tokens")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
    {
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

pub(crate) fn apply_request(request: &mut Value, enabled: bool, id: Option<&str>) {
    if enabled && let Some(id) = response_id(id) {
        request["prompt_cache_options"] = json!({"comparison_response_id":id});
    }
}

pub(crate) fn last_response_id(
    db: &rusqlite::Connection,
    thread: &str,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    // Three PK probes, including for idle Threads; never search growing history.
    db.query_row(
        "SELECT CASE WHEN u.thread_id=t.id AND a.state='completed' AND length(CAST(a.response_id AS BLOB)) BETWEEN 1 AND 1024
         THEN a.response_id END FROM thread t LEFT JOIN model_attempt a
         ON a.id=t.last_adopted_attempt_id LEFT JOIN turn u ON u.id=a.turn_id WHERE t.id=?1",
        rusqlite::params![thread], |row| row.get(0),
    ).optional().map(Option::flatten)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_are_reported_facts_not_inferred_counters() {
        let value = json!({"type":"cache_miss","reason":"some_future_reason","cache_missed_tokens":0,"private":{"large":"ignored"}});
        assert_eq!(
            bounded(&value),
            Some(
                json!({"type":"cache_miss","reason":"some_future_reason","cache_missed_tokens":0})
            )
        );
        assert_eq!(
            bounded(&json!({"type":"future_type"})),
            Some(json!({"type":"future_type"}))
        );
        for value in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"type":false}),
            json!({"type":""}),
            json!({"type":"x".repeat(65)}),
        ] {
            assert_eq!(bounded(&value), None, "{value}");
        }
        for n in [
            json!(-1),
            json!(0.5),
            json!(u64::MAX),
            json!("1"),
            Value::Null,
        ] {
            assert_eq!(
                bounded(&json!({"type":"cache_miss","cache_missed_tokens":n})),
                Some(json!({"type":"cache_miss"}))
            );
        }
        assert_eq!(
            bounded(&json!({"type":"cache_miss","cache_missed_tokens":i64::MAX})).unwrap()["cache_missed_tokens"],
            i64::MAX
        );
    }

    #[test]
    fn diagnostic_bytes_are_bounded_without_truncating_reasons_or_ids() {
        let value = json!({"type":"\0".repeat(64),"reason":"\0".repeat(256),"cache_missed_tokens":i64::MAX});
        assert!(bounded(&value).unwrap().to_string().len() <= MAX_DIAGNOSTIC_BYTES);
        let value =
            json!({"type":"cache_miss","reason":"a".repeat(257),"unknown":"a".repeat(100_000)});
        assert_eq!(bounded(&value), Some(json!({"type":"cache_miss"})));
        assert!(response_id(Some(&"a".repeat(1024))).is_some());
        assert!(response_id(Some(&"a".repeat(1025))).is_none());
        assert!(response_id(Some("")).is_none());
        assert!(response_id(None).is_none());
    }

    #[test]
    fn normalization_removes_missing_or_invalid_diagnostics_without_making_a_hit() {
        for diagnostic in [
            Value::Null,
            json!({"reason":"not a typed diagnostic"}),
            json!([1]),
        ] {
            let mut raw = json!({"usage":{"input_tokens_details":{"cached_tokens":9}},"prompt_cache_diagnostics":diagnostic});
            normalize_response(&mut raw);
            assert!(raw.get("prompt_cache_diagnostics").is_none());
            assert_eq!(raw["usage"]["input_tokens_details"]["cached_tokens"], 9);
        }
    }
}
