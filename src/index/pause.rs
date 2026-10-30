//! Stable saved pause facts; reads never manufacture a stop time.
use super::*;
pub(super) fn save(tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(), StorageError> {
    let cause = |v: &Value| {
        json!([
            v.get("kind"),
            v.get("code"),
            v.get("status"),
            v.get("provider_code")
        ])
    };
    if let Some(old) = json_meta::<Value>(tx, key)?
        && cause(&old) == cause(value)
    {
        return Ok(());
    }
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("paused_at".into(), json!(now()));
    }
    set_json_meta(tx, key, &value)
}
pub(super) fn facts(key: &str, value: &str) -> (Option<String>, String) {
    let at = serde_json::from_str::<Value>(value).ok().and_then(|v| {
        v.get("paused_at")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    (at, hash(format!("{key}\0{value}").as_bytes()))
}
