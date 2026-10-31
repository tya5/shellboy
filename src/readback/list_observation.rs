//! Retained source metadata, not a replacement for canonical CLI list paging.
use crate::{
    protocol::{ApiError, ErrorCode},
    status::observation::{self, TextBudget},
    storage::StorageError,
};
use rusqlite::Connection;
use serde_json::{Value, json};

pub(crate) const SHELL_POINT_STATEMENTS: usize = 1;
pub(crate) const COMPACT_POINT_STATEMENTS: usize = 3;
pub(crate) const COMPACT_PREFIX_STATEMENTS: usize = 3;
#[derive(Default, Debug)]
pub(crate) struct Raw {
    pub rows: Vec<String>,
    pub limited: bool,
    pub unconfirmed: bool,
    pub statements: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Operation,
    Attempt,
    Node,
}
#[derive(Clone, Debug)]
pub(crate) struct RetainedSource {
    pub event_id: String,
    pub source_id: String,
    pub source: Source,
}

pub(crate) const SHELL_POINT_SQL: &str = r#"
SELECT json_object('execution_id',j.value,'available',s.id IS NOT NULL,
 'state',s.state,'thread_id',r.thread_id,'run_id',s.owner_run_id,'turn_id',s.turn_id,
 'started_at',s.started_at,'ended_at',s.ended_at,'exit_code',s.exit_code,
 'saved_sequence',s.saved_sequence,'retention_generation',s.retention_generation,
 'output_complete',s.output_complete,
 'description',CASE WHEN json_valid(s.shell_json) THEN substr(COALESCE(json_extract(s.shell_json,'$.description'),json_extract(s.shell_json,'$.request.description')),1,80) END,
 'command_prefix',CASE WHEN json_valid(s.shell_json) THEN substr(COALESCE(json_extract(s.shell_json,'$.command'),json_extract(s.shell_json,'$.request.command')),1,8192) END,
 'command_prefix_limited',CASE WHEN json_valid(s.shell_json) THEN length(COALESCE(json_extract(s.shell_json,'$.command'),json_extract(s.shell_json,'$.request.command')))>8192 END)
FROM json_each(?1) j LEFT JOIN shell_execution s ON s.id=j.value LEFT JOIN run r ON r.id=s.owner_run_id
"#;
fn read_json_tx(
    db: &Connection,
    sql: &str,
    encoded: &str,
    budget: &mut TextBudget,
) -> Result<Raw, StorageError> {
    let mut result = Raw {
        statements: 1,
        ..Default::default()
    };
    let mut statement = db.prepare(sql)?;
    let mut rows = statement.query([encoded])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > observation::PREFIX {
            result.limited = true;
            break;
        }
        let text = row
            .get_ref(0)?
            .as_str()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        // Borrow SQLite text; refuse the channel before making an owned copy.
        if text.len() > observation::TEXT_BYTES || !budget.take(text.len()) {
            result.limited = true;
            continue;
        }
        result.rows.push(text.to_owned());
    }
    Ok(result)
}
pub(crate) fn read_shells_tx(
    db: &Connection,
    ids: &[String],
    budget: &mut TextBudget,
) -> Result<Raw, StorageError> {
    observation::validate_ids(ids).map_err(StorageError::from)?;
    if ids.is_empty() {
        return Ok(Raw {
            unconfirmed: true,
            ..Default::default()
        });
    }
    let mut raw = read_json_tx(
        db,
        SHELL_POINT_SQL,
        &serde_json::to_string(ids).expect("bounded IDs"),
        budget,
    )?;
    raw.unconfirmed = true;
    assert!(raw.statements <= SHELL_POINT_STATEMENTS);
    Ok(raw)
}

pub(crate) fn compact_point_sql(source: &Source) -> String {
    let (select, joins) = match source {
        Source::Operation => (
            "'available',o.operation_id IS NOT NULL,'source_state',o.state,'created_at',o.created_at,'downstream_available',m.key IS NOT NULL,'downstream_state',CASE WHEN json_valid(m.value) THEN substr(json_extract(m.value,'$.state'),1,64) END,'ended_at',CASE WHEN json_valid(m.value) THEN substr(json_extract(m.value,'$.ended_at'),1,128) END",
            "LEFT JOIN operation_receipt o ON o.operation_id=json_extract(j.value,'$[1]') LEFT JOIN app_meta m ON m.key='compact.result.'||o.operation_id",
        ),
        Source::Attempt => (
            "'available',a.id IS NOT NULL,'source_state',a.state,'turn_id',a.turn_id,'started_at',a.started_at,'ended_at',a.ended_at,'automatic_recorded',CASE WHEN json_valid(a.error_json) THEN json_type(a.error_json,'$.automatic_trim')='object' OR json_type(a.error_json,'$.automatic_trim_attempt')='object' ELSE 0 END",
            "LEFT JOIN model_attempt a ON a.id=json_extract(j.value,'$[1]')",
        ),
        // Existing replacement rows expose output-node IDs, not replacement PKs.
        // Node availability is evidence; absent linkage is not a deleted event.
        Source::Node => (
            "'available',n.id IS NOT NULL,'node_kind',substr(n.kind,1,64),'created_at',n.created_at,'source_revision',n.payload_revision,'body_available',n.payload_json IS NOT NULL,'tombstone_reason',substr(n.deleted_reason,1,512)",
            "LEFT JOIN node n ON n.id=json_extract(j.value,'$[1]')",
        ),
    };
    format!(
        "SELECT json_object('event_id',json_extract(j.value,'$[0]'),'source_id',json_extract(j.value,'$[1]'),{select}) FROM json_each(?1) j {joins}"
    )
}
pub(crate) fn read_compactions_tx(
    db: &Connection,
    sources: &[RetainedSource],
    budget: &mut TextBudget,
) -> Result<Raw, StorageError> {
    if sources.len() > observation::MAX_IDS
        || sources.iter().any(|s| {
            [&s.event_id, &s.source_id]
                .into_iter()
                .any(|id| id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control))
        })
    {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Compact source scope exceeds bounded IDs",
        )
        .into());
    }
    let mut result = Raw {
        unconfirmed: true,
        ..Default::default()
    };
    for source in [Source::Operation, Source::Attempt, Source::Node] {
        let ids = sources
            .iter()
            .filter(|s| s.source == source)
            .map(|s| (&s.event_id, &s.source_id))
            .collect::<Vec<_>>();
        if ids.is_empty() {
            continue;
        }
        let raw = read_json_tx(
            db,
            &compact_point_sql(&source),
            &serde_json::to_string(&ids).expect("bounded source pairs"),
            budget,
        )?;
        result.rows.extend(raw.rows);
        result.limited |= raw.limited;
        result.statements += raw.statements;
    }
    assert!(result.statements <= COMPACT_POINT_STATEMENTS);
    Ok(result)
}

pub(crate) const AUTO: &str = "CASE WHEN json_valid(a.error_json) THEN json_type(a.error_json,'$.automatic_trim')='object' OR json_type(a.error_json,'$.automatic_trim_attempt')='object' ELSE 0 END";
pub(crate) fn compact_prefix_sql(source: &Source, limit: usize) -> String {
    assert!((1..=201).contains(&limit));
    let native = match source {
        Source::Operation => format!(
            "SELECT operation_id id FROM operation_receipt INDEXED BY compact_operation_thread WHERE CASE WHEN json_valid(result_ref_json) THEN json_extract(result_ref_json,'$.result.status')='queued' ELSE 0 END AND json_extract(result_ref_json,'$.result.thread_id')=t.value ORDER BY created_at DESC,operation_id DESC LIMIT {limit}"
        ),
        Source::Attempt => format!(
            "SELECT a.id FROM model_attempt a INDEXED BY automatic_compaction_turn WHERE a.turn_id=(SELECT id FROM turn INDEXED BY one_open_turn WHERE thread_id=t.value AND state IN ('preparing','running','waiting','recovery_wait')) AND {AUTO} ORDER BY a.started_at DESC,a.id DESC LIMIT {limit}"
        ),
        Source::Node => format!(
            "SELECT r.output_node_id id FROM (SELECT replacement_id FROM thread_replacement WHERE thread_id=t.value ORDER BY replacement_id LIMIT {limit}) p CROSS JOIN replacement r ON r.id=p.replacement_id"
        ),
    };
    format!(
        "SELECT json_object('thread_id',t.value,'source_id',p.value) FROM json_each(?1)t CROSS JOIN json_each((SELECT json_group_array(CASE WHEN octet_length(id)<=1024 THEN id END) FROM ({native})))p"
    )
}
pub(crate) fn read_compact_membership_tx(
    db: &Connection,
    threads: &[String],
    budget: &mut TextBudget,
) -> Result<Raw, StorageError> {
    observation::validate_ids(threads).map_err(StorageError::from)?;
    let mut result = Raw {
        unconfirmed: true,
        ..Default::default()
    };
    if threads.is_empty() {
        return Ok(result);
    }
    for source in [Source::Operation, Source::Attempt, Source::Node] {
        let raw = read_json_tx(
            db,
            &compact_prefix_sql(&source, 201 / threads.len()),
            &serde_json::to_string(threads).expect("bounded Threads"),
            budget,
        )?;
        result.rows.extend(raw.rows);
        result.limited |= raw.limited;
        result.statements += 1;
    }
    assert!(result.statements <= COMPACT_PREFIX_STATEMENTS);
    Ok(result)
}
pub(crate) fn prepare(raw: Raw) -> Result<Value, ApiError> {
    let mut values = Vec::with_capacity(raw.rows.len());
    for text in raw.rows {
        let mut value: Value = serde_json::from_str(&text)
            .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Invalid bounded list metadata"))?;
        if let Some(prefix) = value.get("command_prefix").and_then(Value::as_str) {
            let (safe, truncated) = super::shell::snippet(prefix);
            let full = value["command_prefix_limited"]
                .as_bool()
                .or_else(|| value["command_prefix_limited"].as_i64().map(|n| n == 1))
                .unwrap_or(false);
            value["command_excerpt"] = Value::String(safe);
            value["command_truncated"] = Value::Bool(truncated || full);
        }
        if let Some(description) = value.get("description").and_then(Value::as_str) {
            let description = crate::tool_description::label(Some(description))
                .map(|s| super::shell::snippet(&s).0);
            value["description"] = description.map_or(Value::Null, Value::String);
        }
        for name in [
            "available",
            "downstream_available",
            "automatic_recorded",
            "body_available",
            "output_complete",
        ] {
            if let Some(number) = value.get(name).and_then(Value::as_i64) {
                if number == 0 || number == 1 {
                    value[name] = Value::Bool(number == 1);
                }
            }
        }
        if let Some(object) = value.as_object_mut() {
            object.remove("command_prefix");
            object.remove("command_prefix_limited");
        }
        values.push(value);
    }
    Ok(
        json!({"rows":values,"limited":raw.limited,"membership_unconfirmed":raw.unconfirmed,"sql_statements":raw.statements}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepare_never_exposes_the_unsanitized_command_prefix() {
        let raw=Raw{rows:vec![json!({"execution_id":"s","command_prefix":"TOKEN=private echo visible","command_prefix_limited":true}).to_string()],..Default::default()};
        let value = prepare(raw).unwrap();
        assert!(!value.to_string().contains("private"));
        assert!(value["rows"][0].get("command_prefix").is_none());
        assert_eq!(value["rows"][0]["command_truncated"], true);
    }
    #[test]
    fn invalid_and_excessive_source_scopes_fail_without_queries() {
        let db = Connection::open_in_memory().unwrap();
        let sources = (0..201)
            .map(|n| RetainedSource {
                event_id: n.to_string(),
                source_id: n.to_string(),
                source: Source::Node,
            })
            .collect::<Vec<_>>();
        assert!(read_compactions_tx(&db, &sources, &mut TextBudget::default()).is_err());
    }
}
