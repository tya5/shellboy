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
    let sql = comparison_query();
    db.query_row(&sql, rusqlite::params![thread], |row| row.get(0))
        .optional()
        .map(Option::flatten)
}

fn comparison_query() -> String {
    format!(
        "WITH {} SELECT CASE WHEN octet_length(a.response_id) BETWEEN 1 AND 1024
         THEN a.response_id END FROM selected
         JOIN model_attempt a ON a.rowid=selected.native_rowid",
        crate::compaction::context_usage::latest_completed_cte()
    )
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

#[cfg(test)]
mod bounded_comparison_tests {
    use super::*;
    use rusqlite::{Connection, StatementStatus, params};

    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');").unwrap();
        db
    }
    fn turn(db: &Connection, id: &str) {
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,'t','r','completed',?1)",[id]).unwrap();
    }
    fn attempt(db: &Connection, id: &str, turn: &str, state: &str, response: Option<&str>) {
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,response_id,started_at)VALUES(?1,?2,0,0,'p','m',0,?3,'{}',?4,'now')",params![id,turn,state,response]).unwrap();
    }
    #[test]
    fn cache_comparison_is_exactly_the_shared_33_turn_window_and_never_falls_back() {
        let db = fixture();
        assert_eq!(crate::storage::SCHEMA_VERSION, 26);
        assert_eq!(last_response_id(&db, "t").unwrap(), None);
        for n in 1..=33 {
            turn(&db, &format!("z{n:02}"));
        }
        attempt(&db, "first", "z01", "completed", Some("edge"));
        assert_eq!(last_response_id(&db, "t").unwrap().as_deref(), Some("edge"));
        turn(&db, "z34");
        assert_eq!(last_response_id(&db, "t").unwrap(), None);
        let mut body = json!({});
        apply_request(
            &mut body,
            true,
            last_response_id(&db, "t").unwrap().as_deref(),
        );
        assert!(body.get("prompt_cache_options").is_none());
        attempt(&db, "inside", "z02", "completed", Some("inside"));
        for state in ["failed", "incomplete", "cancelled", "inflight"] {
            attempt(&db, state, "z34", state, Some("not-adopted"));
        }
        assert_eq!(
            last_response_id(&db, "t").unwrap().as_deref(),
            Some("inside")
        );
        attempt(&db, "missing", "z33", "completed", None);
        assert_eq!(last_response_id(&db, "t").unwrap(), None);
        db.execute(
            "UPDATE model_attempt SET response_id=?1 WHERE id='missing'",
            ["x".repeat(1025)],
        )
        .unwrap();
        assert_eq!(last_response_id(&db, "t").unwrap(), None);
        db.execute(
            "UPDATE model_attempt SET response_id='last' WHERE id='missing'",
            [],
        )
        .unwrap();
        assert_eq!(last_response_id(&db, "t").unwrap().as_deref(), Some("last"));
        db.execute("UPDATE thread SET model_id='other' WHERE id='t'", [])
            .unwrap();
        assert_eq!(last_response_id(&db, "t").unwrap(), None);
        db.pragma_update(None, "query_only", true).unwrap();
        let before = db.total_changes();
        assert_eq!(last_response_id(&db, "unknown-thread").unwrap(), None);
        assert_eq!(last_response_id(&db, "t").unwrap(), None);
        assert_eq!(db.total_changes(), before);
    }
    #[test]
    fn cache_comparison_index_seeks_and_vm_work_ignore_older_history_and_attempt_depth() {
        let mut db = fixture();
        for n in 1..=33 {
            turn(&db, &format!("z{n:02}"));
        }
        attempt(&db, "witness", "z33", "completed", Some("last"));
        let measure = |db: &Connection| {
            let mut q = db.prepare(&comparison_query()).unwrap();
            assert_eq!(
                q.query_row(["t"], |r| r.get::<_, String>(0)).unwrap(),
                "last"
            );
            q.get_status(StatementStatus::VmStep)
        };
        let small = measure(&db);
        let tx = db.transaction().unwrap();
        for n in 0..5000 {
            let id = format!("old{n:05}");
            turn(&tx, &id);
            attempt(&tx, &format!("a{n:05}"), &id, "completed", Some("older"));
            attempt(
                &tx,
                &format!("deep{n:05}"),
                "z33",
                "completed",
                Some("last"),
            );
            // Add matching and failed depth while keeping the reported ID unchanged.
            attempt(
                &tx,
                &format!("failed{n:05}"),
                "z33",
                "failed",
                Some("ignored"),
            );
        }
        tx.commit().unwrap();
        let large = measure(&db);
        assert!(
            large <= small + 20,
            "VM steps grew with old history: {small} -> {large}"
        );
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {}", comparison_query()))
            .unwrap()
            .query_map(["t"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter().any(|p| p.contains("attempts_completed_scope")
                && p.contains("turn_id=? AND profile_id=? AND model_id=?")),
            "{plan:?}"
        );
        assert!(!plan.iter().any(|p| p.contains("SCAN a")), "{plan:?}");
    }
}
