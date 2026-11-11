//! Private bounded projection foundation; not yet published on history responses.
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

/// This wrapper, not the public DTO's derive, is the strict admission boundary.
#[allow(dead_code)]
pub(super) struct ValidatedNotification(crate::protocol::NotificationDescriptor);
impl ValidatedNotification {
    #[allow(dead_code)]
    pub(super) fn into_descriptor(self) -> crate::protocol::NotificationDescriptor {
        self.0
    }
}
#[allow(dead_code)]
pub(super) fn descriptor(
    db: &Connection,
    node: &str,
) -> rusqlite::Result<Option<ValidatedNotification>> {
    Ok(snapshot(db, node)?
        .and_then(|value| serde_json::from_value(value).ok())
        .map(ValidatedNotification))
}

// Preserve duplicate-key evidence at every depth before building a JSON Value.
struct UniqueJson(Value);
impl<'de> serde::Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueJson;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate object keys")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut object = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if object.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate object key"));
                    }
                    object.insert(key, map.next_value::<UniqueJson>()?.0);
                }
                Ok(UniqueJson(Value::Object(object)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<UniqueJson>()? {
                    values.push(value.0);
                }
                Ok(UniqueJson(Value::Array(values)))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(UniqueJson(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueJson(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::String(v.into())))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::String(v)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::Null))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[allow(dead_code)]
pub(super) fn snapshot(db: &Connection, node: &str) -> rusqlite::Result<Option<Value>> {
    let raw: Option<Option<String>> = db.query_row(
        "SELECT CASE WHEN length(CAST(e.semantic_json AS BLOB))<=2048 THEN e.semantic_json END FROM node n JOIN turn t ON t.id=n.turn_id JOIN event e ON e.id=n.notification_event_id WHERE n.id=?1 AND n.kind='event' AND t.thread_id=e.target_thread_id",
        [node],|r|r.get(0)
    ).optional()?;
    Ok(raw.flatten().and_then(|raw| decode(&raw)))
}
pub(crate) fn decode(raw: &str) -> Option<Value> {
    if raw.len() > 2048 {
        return None;
    }
    let value = serde_json::from_str::<UniqueJson>(raw).ok()?.0;
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
    serde_json::from_value::<crate::protocol::NotificationDescriptor>(value.clone()).ok()?;
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dto_deserialization_is_not_the_strict_admission_boundary() {
        for raw in [
            r#"{"kind":"shell_ended","execution_id":"e","state":"exited","exit_code":null}"#,
            r#"{"kind":"shell_ended","execution_id":" ","state":"exited"}"#,
            r#"{"kind":"thread_settings_changed","changed_fields":["cwd","cwd"]}"#,
        ] {
            assert!(serde_json::from_str::<crate::protocol::NotificationDescriptor>(raw).is_ok());
            assert!(decode(raw).is_none());
        }
    }
    #[test]
    fn actual_schema_upgrade_converges_and_marker_fk_preserves_event() {
        let schema = include_str!("../storage/current.sql");
        let predecessor = schema
            .split("ALTER TABLE event ADD COLUMN semantic_json TEXT;")
            .next()
            .unwrap();
        let fresh = Connection::open_in_memory().unwrap();
        fresh.execute_batch(schema).unwrap();
        let upgraded = Connection::open_in_memory().unwrap();
        upgraded.execute_batch(predecessor).unwrap();
        upgraded
            .execute_batch(include_str!(
                "../../migrations/0042_notification_semantic_descriptor.sql"
            ))
            .unwrap();
        let objects = |db: &Connection| {
            db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").unwrap().query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        assert_eq!(objects(&fresh), objects(&upgraded));
        fresh.execute_batch("PRAGMA foreign_keys=ON; INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('creator','/synthetic','/synthetic','p','m','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('r','creator','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('t','creator','r','completed','now'); INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,semantic_json,created_at) VALUES('e','context_changed','{}','creator','{}','{\"kind\":\"turn_started\"}','now'); INSERT INTO node(id,turn_id,kind,created_at,payload_json,notification_event_id) VALUES('n','t','event','now','{}','e'); INSERT INTO delivery(event_id,recipient_id,state) VALUES('e','r','pending');").unwrap();
        let original = snapshot(&fresh, "n").unwrap().unwrap();
        fresh.execute("DELETE FROM delivery", []).unwrap();
        assert_eq!(snapshot(&fresh, "n").unwrap(), Some(original));
        assert!(fresh.execute("DELETE FROM event WHERE id='e'", []).is_err());
        let plans=fresh.prepare("EXPLAIN QUERY PLAN SELECT CASE WHEN length(CAST(e.semantic_json AS BLOB))<=2048 THEN e.semantic_json END FROM node n JOIN turn t ON t.id=n.turn_id JOIN event e ON e.id=n.notification_event_id WHERE n.id=?1 AND n.kind='event' AND t.thread_id=e.target_thread_id").unwrap().query_map(["n"],|r|r.get::<_,String>(3)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert!(plans.iter().all(|plan| !plan.contains("SCAN")), "{plans:?}");
        assert_eq!(
            fresh
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
    }
    #[test]
    fn duplicate_keys_fail_before_value_normalization_at_every_depth() {
        for raw in [
            r#"{"kind":"future","kind":"turn_started"}"#,
            r#"{"kind":"future","k\u0069nd":"turn_started"}"#,
            r#"{"kind":"shell_ended","execution_id":"secret","execution_id":"public","state":"exited"}"#,
        ] {
            assert!(decode(raw).is_none());
        }
        for raw in [
            r#"{"nested":{"key":1,"key":2}}"#,
            r#"[{"nested":{"key":1,"key":2}}]"#,
        ] {
            assert!(serde_json::from_str::<UniqueJson>(raw).is_err());
        }
        assert!(decode(r#"{"kind":"turn_started"} {}"#).is_none());
        assert!(decode(r#"{"kind":"turn_started"}"#).is_some());
    }
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
