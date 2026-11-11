//! Private bounded projection foundation; not yet published on history responses.
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

#[allow(dead_code)]
pub(super) fn snapshot(db: &Connection, node: &str) -> rusqlite::Result<Option<Value>> {
    let raw: Option<Option<String>> = db.query_row(
        "SELECT CASE WHEN length(CAST(e.semantic_json AS BLOB))<=2048 THEN e.semantic_json END FROM node n JOIN turn t ON t.id=n.turn_id JOIN event e ON e.id=n.notification_event_id WHERE n.id=?1 AND n.kind='event' AND t.thread_id=e.target_thread_id",
        [node],|r|r.get(0)
    ).optional()?;
    Ok(raw.flatten().and_then(|raw| decode(&raw)))
}
fn decode(raw: &str) -> Option<Value> {
    if raw.len() > 2048 {
        return None;
    }
    let value: Value = serde_json::from_str(raw).ok()?;
    let fields = value.as_object()?;
    let kind = fields.get("kind")?.as_str()?;
    let allowed: &[&str] = match kind {
        "turn_started" | "recovery_resumed" => &["kind"],
        "instructions_observed" => &["kind", "observation"],
        "thread_settings_changed" => &["kind", "changed_fields"],
        "shell_ended" => &["kind", "execution_id", "state", "exit_code"],
        _ => return None,
    };
    if fields.keys().any(|key| !allowed.contains(&key.as_str()))
        || fields.values().any(Value::is_null)
    {
        return None;
    }
    match kind {
        "instructions_observed" => {
            if !matches!(
                fields.get("observation")?.as_str()?,
                "first_seen" | "updated" | "removed"
            ) {
                return None;
            }
        }
        "thread_settings_changed" => {
            let entries = fields.get("changed_fields")?.as_array()?;
            if entries.is_empty() || entries.len() > 4 {
                return None;
            }
            let mut seen = std::collections::HashSet::with_capacity(4);
            for entry in entries {
                let field = entry.as_str()?;
                if !matches!(field, "cwd" | "connection" | "model" | "effort")
                    || !seen.insert(field)
                {
                    return None;
                }
            }
        }
        "shell_ended" => {
            let id = fields.get("execution_id")?.as_str()?;
            if id.trim().is_empty() || id.len() > 256 {
                return None;
            }
            if !matches!(
                fields.get("state")?.as_str()?,
                "exited" | "failed" | "stopped"
            ) {
                return None;
            }
            if let Some(code) = fields.get("exit_code") {
                i32::try_from(code.as_i64()?).ok()?;
            }
        }
        _ => {}
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_snapshot_rejects_unknown_null_and_secret_fields() {
        for raw in [
            r#"{"kind":"turn_started","secret":"sentinel"}"#,
            r#"{"kind":"future"}"#,
            r#"{"kind":"instructions_observed","observation":null}"#,
            r#"{"kind":"thread_settings_changed","changed_fields":["cwd","cwd"]}"#,
        ] {
            assert!(decode(raw).is_none());
        }
        assert!(decode(r#"{"kind":"turn_started"}"#).is_some());
    }
    #[test]
    fn historical_snapshot_survives_delivery_absence_and_rejects_foreign_creator() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE node(id TEXT,kind TEXT,turn_id TEXT,notification_event_id TEXT); CREATE TABLE turn(id TEXT,thread_id TEXT); CREATE TABLE event(id TEXT,target_thread_id TEXT,semantic_json TEXT); INSERT INTO turn VALUES('t','creator'); INSERT INTO event VALUES('e','creator','{\"kind\":\"turn_started\"}'); INSERT INTO node VALUES('n','event','t','e');").unwrap();
        assert!(snapshot(&db, "n").unwrap().is_some());
        db.execute("UPDATE turn SET thread_id='foreign'", [])
            .unwrap();
        assert!(snapshot(&db, "n").unwrap().is_none());
        db.execute("UPDATE node SET notification_event_id=NULL", [])
            .unwrap();
        assert!(snapshot(&db, "n").unwrap().is_none());
    }
}
