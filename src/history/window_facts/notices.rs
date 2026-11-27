//! Owner read of saved notification metadata for window observation.
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
};
use rusqlite::Connection;

// Requested IDs stay outermost. Every relation is reached through an existing
// primary/unique key; a duplicate call ID in another group cannot supply facts.
pub(crate) const SQL: &str = r"
WITH requested AS (SELECT DISTINCT value AS node_id FROM json_each(?1)),
events AS MATERIALIZED (
 SELECT n.id AS node_id,e.kind,e.payload_ref_json AS payload
 FROM requested CROSS JOIN node n ON n.id=requested.node_id
 JOIN event e ON e.id=CASE WHEN json_valid(n.payload_json) THEN json_extract(n.payload_json,'$.event_id') END
 WHERE n.kind='event' AND e.kind='shell_ended'
), refs AS MATERIALIZED (
 SELECT events.*,CASE WHEN json_valid(payload) THEN json_extract(payload,'$.execution_id') END AS execution_id FROM events
)
SELECT refs.node_id,refs.kind,refs.execution_id,
 substr(CASE WHEN json_valid(c.arguments_json) THEN CASE WHEN json_type(c.arguments_json,'$.command')='text' THEN json_extract(c.arguments_json,'$.command') END END,1,120),
 CASE WHEN json_valid(c.arguments_json) THEN CASE WHEN json_type(c.arguments_json,'$.background') IN ('true','false') THEN json_extract(c.arguments_json,'$.background') END END,
 c.group_id,c.call_id,r.node_id,
 s.id,original.deleted_reason,
 substr(CASE WHEN json_valid(c.arguments_json) THEN CASE WHEN json_type(c.arguments_json,'$.description')='text' THEN json_extract(c.arguments_json,'$.description') END END,1,512)
FROM refs
LEFT JOIN shell_execution s ON s.id=refs.execution_id
LEFT JOIN tool_call c ON c.operation_id=s.id AND EXISTS(SELECT 1 FROM tool_group g WHERE g.id=c.group_id AND g.turn_id=s.turn_id)
LEFT JOIN tool_result r ON r.group_id=c.group_id AND r.call_id=c.call_id
LEFT JOIN node original ON original.id=r.node_id
";

pub(crate) use crate::protocol::HistoryWindowNotice as Fact;

pub(crate) fn read_tx(db: &Connection, ids: &[String]) -> Result<Vec<Fact>, StorageError> {
    if ids.len() > 128 || ids.iter().any(|id| id.is_empty() || id.len() > 256) {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Notification source scope exceeds128 nodes",
        )
        .into());
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let encoded = serde_json::to_string(ids).expect("notification IDs serialize");
    db.prepare(SQL)?
        .query_map([encoded], |r| {
            let source: Option<String> = r.get(7)?;
            let source_deleted: Option<String> = r.get(9)?;
            Ok(Fact {
                node_id: r.get(0)?,
                execution_id: r.get(2)?,
                command: r.get(3)?,
                background: r.get(4)?,
                group_id: r.get(5)?,
                call_id: r.get(6)?,
                source_node_id: source.filter(|_| source_deleted.is_none()),
                execution_available: r.get::<_, Option<String>>(8)?.is_some(),
                description: r.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}
