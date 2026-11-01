//! Opaque provider items (#120 stage 2): content Shellboy keeps and returns but
//! never interprets (for example Anthropic `thinking` with its signature). The
//! envelope is the only shape the core checks; `block` is never read.
use serde_json::{Map, Value, json};

pub const TYPE: &str = "provider_opaque";
const MAX_NAME_BYTES: usize = 64;

pub fn is_opaque(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some(TYPE)
}

/// Build the envelope a dialect decoder stores for a block it will not interpret.
pub fn wrap(dialect: &str, kind: &str, block: Value) -> Value {
    json!({"type":TYPE,"dialect":dialect,"kind":kind,"block":block})
}

fn valid_name(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= MAX_NAME_BYTES && name.is_ascii())
}

/// Check the envelope shape only. Returns the dialect it belongs to.
pub fn validate(item: &Value) -> Result<&str, &'static str> {
    let object: &Map<String, Value> = item.as_object().ok_or("Opaque item is not an object")?;
    let dialect = valid_name(object.get("dialect")).ok_or("Opaque item has no valid dialect")?;
    valid_name(object.get("kind")).ok_or("Opaque item has no valid kind")?;
    if !object.get("block").is_some_and(Value::is_object) {
        return Err("Opaque item block must be an object");
    }
    Ok(dialect)
}

/// The block a dialect's encoder puts back on the wire, unchanged. Items of any
/// other dialect, and malformed envelopes, yield `None` and are skipped.
pub fn block_for<'a>(item: &'a Value, dialect: &str) -> Option<&'a Value> {
    (is_opaque(item) && validate(item) == Ok(dialect))
        .then(|| item.get("block"))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_strings_byte_exact_and_selects_by_dialect() {
        let block = json!({"type":"thinking","thinking":"日本語 \u{1F980} \"q\" \\ \n","signature":"EqQB/+=="});
        let item = wrap("anthropic_messages", "thinking", block.clone());
        assert!(is_opaque(&item));
        assert_eq!(validate(&item), Ok("anthropic_messages"));
        // Save and reload through serde as the history does.
        let saved = serde_json::to_string(&item).unwrap();
        let loaded: Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(block_for(&loaded, "anthropic_messages"), Some(&block));
        assert_eq!(
            block_for(&loaded, "anthropic_messages").unwrap()["signature"],
            "EqQB/+=="
        );
        assert_eq!(block_for(&loaded, "chat_completions"), None);
        assert_eq!(block_for(&loaded, "responses"), None);
    }

    #[test]
    fn malformed_envelopes_are_rejected_and_never_sent() {
        for bad in [
            json!({"type":TYPE,"kind":"k","block":{}}),
            json!({"type":TYPE,"dialect":"","kind":"k","block":{}}),
            json!({"type":TYPE,"dialect":"d","block":{}}),
            json!({"type":TYPE,"dialect":"d","kind":"k","block":"text"}),
            json!({"type":TYPE,"dialect":"d","kind":"k","block":[1]}),
            json!({"type":TYPE,"dialect":"日本","kind":"k","block":{}}),
            json!({"type":TYPE,"dialect":"x".repeat(65),"kind":"k","block":{}}),
            json!({"type":TYPE,"dialect":"d","kind":"k".repeat(65),"block":{}}),
        ] {
            assert!(validate(&bad).is_err(), "{bad}");
            assert_eq!(block_for(&bad, "d"), None, "{bad}");
        }
        assert!(validate(&json!("text")).is_err());
        assert!(!is_opaque(&json!({"type":"reasoning"})));
    }
}
