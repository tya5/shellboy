//! Spill-local saved provenance only: no traversal, global call-ID lookup or payload hydration.
use crate::protocol::{ApiError, SpillToolSource, SpillToolSources};
use rusqlite::Connection;
use serde_json::Value;

pub(crate) const MAX_SOURCES: usize = 8;
const MAX_ID_BYTES: usize = 256;
// One point lookup. The fixed projection bounds bytes returned even for corrupt legacy metadata.
pub(crate) const LOOKUP_SQL: &str =
    "SELECT substr(json_extract(payload_json,'$.spill_tools'),1,16385) FROM node WHERE id=?1";

pub(crate) fn from_outputs(output: &[Value]) -> SpillToolSources {
    let mut items = Vec::new();
    let mut truncated = false;
    for item in output {
        if item["type"] != "function_call_output" {
            continue;
        }
        let Some(call_id) = identifier(item.get("call_id")) else {
            truncated = true;
            continue;
        };
        let saved = item["output"]
            .as_str()
            .filter(|s| s.len() <= 4096)
            .and_then(|s| serde_json::from_str::<Value>(s).ok());
        let saved = saved.as_ref();
        if items.len() == MAX_SOURCES {
            truncated = true;
            break;
        }
        items.push(SpillToolSource {
            call_id,
            execution_id: saved.and_then(|s| identifier(s.get("execution_id"))),
            original_node_id: saved.and_then(|s| identifier(s.get("original_node_id"))),
        });
    }
    SpillToolSources {
        status: if items.is_empty() { "unknown" } else { "known" }.into(),
        items,
        truncated,
    }
}
fn identifier(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= MAX_ID_BYTES)
        .map(str::to_owned)
}
pub(crate) fn load(db: &Connection, id: &str) -> Result<SpillToolSources, ApiError> {
    let raw: Option<String> = db
        .query_row(LOOKUP_SQL, [id], |r| r.get(0))
        .map_err(super::sql_error)?;
    let unknown = || SpillToolSources {
        status: "unknown".into(),
        items: Vec::new(),
        truncated: false,
    };
    let Some(raw) = raw else {
        return Ok(unknown());
    };
    if raw.len() > 16384 {
        return Ok(SpillToolSources {
            truncated: true,
            ..unknown()
        });
    }
    let Ok(mut saved) = serde_json::from_str::<SpillToolSources>(&raw) else {
        return Ok(unknown());
    };
    saved.truncated |= saved.items.len() > MAX_SOURCES;
    saved.items.truncate(MAX_SOURCES);
    if saved.items.iter().any(|item| {
        identifier(Some(&Value::String(item.call_id.clone()))).is_none()
            || item
                .execution_id
                .as_ref()
                .is_some_and(|s| s.len() > MAX_ID_BYTES)
            || item
                .original_node_id
                .as_ref()
                .is_some_and(|s| s.len() > MAX_ID_BYTES)
    }) {
        return Ok(unknown());
    }
    saved.status = if saved.items.is_empty() {
        "unknown"
    } else {
        "known"
    }
    .into();
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn spill_tool_sources_bound_output_and_keep_missing_execution_unknown() {
        let output=(0..10).map(|i|json!({"type":"function_call_output","call_id":format!("call-{i}"),"output":json!({"execution_id":format!("exec-{i}"),"original_node_id":format!("node-{i}")}).to_string()})).collect::<Vec<_>>();
        let sources = from_outputs(&output);
        assert_eq!(sources.status, "known");
        assert_eq!(sources.items.len(), 8);
        assert!(sources.truncated);
        assert_eq!(sources.items[0].execution_id.as_deref(), Some("exec-0"));
        let unknown = from_outputs(&[
            json!({"type":"function_call_output","call_id":"call","output":"not json"}),
        ]);
        assert_eq!(unknown.items[0].call_id, "call");
        assert!(unknown.items[0].execution_id.is_none());
    }
    #[test]
    fn spill_tool_sources_lookup_is_one_index_seek_and_bounded_legacy_projection() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE node(id TEXT PRIMARY KEY,payload_json TEXT);")
            .unwrap();
        let sources = from_outputs(&[
            json!({"type":"function_call_output","call_id":"a","output":"{\"execution_id\":\"e\"}"}),
        ]);
        db.execute(
            "INSERT INTO node VALUES('s',?1)",
            [json!({"spill_tools":sources}).to_string()],
        )
        .unwrap();
        assert_eq!(
            load(&db, "s").unwrap().items[0].execution_id.as_deref(),
            Some("e")
        );
        db.execute("INSERT INTO node VALUES('old','{}')", [])
            .unwrap();
        assert_eq!(load(&db, "old").unwrap().status, "unknown");
        db.execute(
            "UPDATE node SET payload_json=?1 WHERE id='old'",
            [json!({"spill_tools":"x".repeat(20_000)}).to_string()],
        )
        .unwrap();
        assert!(load(&db, "old").unwrap().truncated);
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {LOOKUP_SQL}"))
            .unwrap()
            .query_map(["s"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter().any(|s| s.contains("SEARCH node USING INDEX")),
            "{plan:?}"
        );
        assert!(!plan.iter().any(|s| s.contains("SCAN")), "{plan:?}");
    }
}
