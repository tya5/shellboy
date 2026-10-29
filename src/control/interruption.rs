//! Bounded pre-terminal context captured by startup and shutdown transactions.
use crate::storage::StorageError;

use serde_json::{Value, json};

const PREVIEW_ITEMS: usize = 64;
const DETAIL_REFERENCES: usize = 64;
const DETAIL_ITEMS: usize = 32;
const DETAIL_INPUT_BYTES: usize = 65_536;
const WAIT_PREVIEW_CONDITIONS: usize = 4;

const THREADS_WITH_INTERRUPTED_WORK: &str = r"SELECT id
FROM thread t
WHERE deleted_at IS NULL
AND (EXISTS (
        SELECT 1
        FROM run r
        WHERE r.thread_id = t.id
        AND r.state IN ('live', 'stopping'))
    OR EXISTS (
        SELECT 1
        FROM turn u
        WHERE u.thread_id = t.id
        AND u.state IN ('preparing', 'running', 'waiting', 'recovery_wait'))
    OR EXISTS (
        SELECT 1 FROM tool_group g JOIN turn u ON u.id=g.turn_id
        WHERE u.thread_id=t.id AND (g.state<>'completed' OR EXISTS (
            SELECT 1 FROM tool_call c WHERE c.group_id=g.id
            AND (c.state IN ('pending','running') OR NOT EXISTS (
                SELECT 1 FROM tool_result z WHERE z.group_id=c.group_id AND z.call_id=c.call_id)))))
    OR EXISTS (
        SELECT 1
        FROM app_meta m
        WHERE m.key LIKE 'wait.pending.%'
        AND (json_extract(m.value, '$.caller.thread_id') = t.id
            OR (json_extract(m.value, '$.caller') IS NULL
                AND EXISTS (
                    SELECT 1
                    FROM json_each(m.value, '$.conditions') c
                    WHERE json_extract(c.value, '$.run_id') IN (
                        SELECT id
                        FROM run
                        WHERE thread_id = t.id)
                    OR json_extract(c.value, '$.turn_id') IN (
                        SELECT id
                        FROM turn
                        WHERE thread_id = t.id)
                    OR json_extract(c.value, '$.execution_id') IN (
                        SELECT s.id
                        FROM shell_execution s
                        JOIN run r ON r.id = s.owner_run_id
                        WHERE r.thread_id = t.id))))))
ORDER BY id";

const RUN_SNAPSHOT: &str = r"SELECT json_object('run_id', id,
    'owner_run_id', owner_run_id,
    'request_count', (
        SELECT count(*)
        FROM receipt
        WHERE run_id = run.id),
    'first_message_id', (
        SELECT id
        FROM receipt
        WHERE run_id = run.id
        ORDER BY rowid LIMIT 1),
    'first_request_node_id', (
        SELECT message_node_id
        FROM receipt
        WHERE run_id = run.id
        ORDER BY rowid LIMIT 1),
    'previous_state', state,
    'startup_state', 'stopped',
    'stop_reason', stop_reason)
FROM run
WHERE thread_id = ?1
AND state IN ('live', 'stopping')
ORDER BY created_at, id";

const TURN_SNAPSHOT: &str = r"SELECT json_object('turn_id', u.id,
    'run_id', u.run_id,
    'previous_state', u.state,
    'answer_node_id', u.answer_node_id,
    'recovery_reason', json(u.recovery_reason_json))
FROM turn u
JOIN run r ON r.id = u.run_id
WHERE u.thread_id = ?1
AND (u.state IN ('preparing', 'running', 'waiting', 'recovery_wait')
    OR (r.state IN ('live', 'stopping')
        AND u.id = (
            SELECT z.id
            FROM turn z
            WHERE z.run_id = r.id
            ORDER BY z.created_at DESC, z.rowid DESC LIMIT 1)))
ORDER BY u.created_at, u.id";

const SHELL_SNAPSHOT: &str = r"SELECT json_object('execution_id', s.id,
    'run_id', s.owner_run_id,
    'turn_id', s.turn_id,
    'previous_state', s.state,
    'startup_state', 'stopped',
    'output_complete', json('false'),
    'saved_sequence', s.saved_sequence,
    'retention_generation', s.retention_generation,
    'result_uncertain', json('true'))
FROM shell_execution s
JOIN run r ON r.id = s.owner_run_id
WHERE r.thread_id = ?1
AND s.state IN ('starting', 'running', 'stopping')
ORDER BY s.id";

const TOOL_CALL_SNAPSHOT: &str = r"SELECT json_object(
    'group_id', g.id, 'call_id', c.call_id,
    'run_id', u.run_id, 'turn_id', u.id,
    'previous_state', c.state, 'previous_group_state', g.state,
    'execution_id', s.id,
    'result_saved', json(CASE WHEN z.node_id IS NULL THEN 'false' ELSE 'true' END),
    'repair_outcome', CASE WHEN z.node_id IS NOT NULL THEN 'preserved_result'
        WHEN c.state='pending' AND c.operation_id IS NULL THEN 'not_started' ELSE 'result_uncertain' END)
FROM tool_group g JOIN turn u ON u.id=g.turn_id
JOIN tool_call c ON c.group_id=g.id
LEFT JOIN tool_result z ON z.group_id=c.group_id AND z.call_id=c.call_id
LEFT JOIN shell_execution s ON s.id=c.operation_id AND s.owner_run_id=u.run_id
WHERE u.thread_id=?1 AND (g.state<>'completed' OR EXISTS (
    SELECT 1 FROM tool_call pending WHERE pending.group_id=g.id
    AND (pending.state IN ('pending','running') OR NOT EXISTS (
        SELECT 1 FROM tool_result saved WHERE saved.group_id=pending.group_id AND saved.call_id=pending.call_id))))
ORDER BY g.created_at,g.id,c.ordinal";

const OWNED_CHILDREN_SNAPSHOT: &str = r"SELECT json_object('run_id', c.id,
    'thread_id', c.thread_id,
    'owner_run_id', c.owner_run_id,
    'first_message_id', (
        SELECT id
        FROM receipt
        WHERE run_id = c.id
        ORDER BY rowid LIMIT 1),
    'first_request_node_id', (
        SELECT message_node_id
        FROM receipt
        WHERE run_id = c.id
        ORDER BY rowid LIMIT 1),
    'previous_state', c.state,
    'startup_state', CASE WHEN c.state IN ('live', 'stopping') THEN 'stopped' ELSE c.state END,
    'last_turn_id', (
        SELECT u.id
        FROM turn u
        WHERE u.run_id = c.id
        ORDER BY u.created_at DESC, u.rowid DESC LIMIT 1),
    'answer_node_id', (
        SELECT u.answer_node_id
        FROM turn u
        WHERE u.run_id = c.id
        ORDER BY u.created_at DESC, u.rowid DESC LIMIT 1))
FROM run c
JOIN run p ON p.id = c.owner_run_id
WHERE p.thread_id = ?1
AND (p.state IN ('live', 'stopping')
    OR c.state IN ('live', 'stopping'))
ORDER BY c.created_at, c.id";

const MESSAGE_SNAPSHOT: &str = r"SELECT json_object('receipt_id', id,
    'message_node_id', message_node_id,
    'run_id', run_id,
    'turn_id', turn_id,
    'previous_state', state,
    'assignment_preserved', json('true'))
FROM receipt
WHERE target_thread_id = ?1
AND state IN ('accepted', 'held')
ORDER BY rowid";

const WAIT_SNAPSHOT: &str = r"SELECT json_object('operation_id', json_extract(m.value, '$.operation_id'),
    'caller', json_object('thread_id', json_extract(m.value, '$.caller.thread_id'),
        'run_id', json_extract(m.value, '$.caller.run_id'),
        'turn_id', json_extract(m.value, '$.caller.turn_id'),
        'execution_id', json_extract(m.value, '$.caller.execution_id')),
    'conditions', json_extract(m.value, '$.conditions'),
    'timeout_ms', json_extract(m.value, '$.timeout_ms'),
    'accepted_at', json_extract(m.value, '$.accepted_at'),
    'observation_state', 'interrupted')
FROM app_meta m
WHERE m.key LIKE 'wait.pending.%'
AND (json_extract(m.value, '$.caller.thread_id') = ?1
    OR (json_extract(m.value, '$.caller') IS NULL
        AND EXISTS (
            SELECT 1
            FROM json_each(m.value, '$.conditions') c
            WHERE json_extract(c.value, '$.run_id') IN (
                SELECT id
                FROM run
                WHERE thread_id = ?1)
            OR json_extract(c.value, '$.turn_id') IN (
                SELECT id
                FROM turn
                WHERE thread_id = ?1)
            OR json_extract(c.value, '$.execution_id') IN (
                SELECT s.id
                FROM shell_execution s
                JOIN run r ON r.id = s.owner_run_id
                WHERE r.thread_id = ?1))))
ORDER BY m.key";

const PENDING_UNKNOWN_SNAPSHOT: &str = r"SELECT json_object('operation_id', o.operation_id,
    'request_kind', 'unknown',
    'wait_targets', 'unknown',
    'state', o.state)
FROM operation_receipt o
WHERE o.state = 'pending'
AND json_extract(o.origin_json, '$.thread_id') = ?1
AND NOT EXISTS (
    SELECT 1
    FROM app_meta m
    WHERE m.key = 'wait.pending.' || o.operation_id)
ORDER BY o.created_at, o.operation_id";

const APPEND_CONTEXT_EVENT: &str = r"INSERT INTO event(id, kind, source_json, target_thread_id, payload_ref_json, created_at) VALUES(?1, 'context_changed', ?2, ?3, ?4, ?5)";

/// Owns each section's bounded preview and independently pageable originals.
struct SectionWriter<'a, 'db> {
    tx: &'a rusqlite::Transaction<'db>,
    thread: &'a str,
    at: &'a str,
    section: &'a str,
    reason: &'a str,
    items: Vec<Value>,
    count: usize,
    chunk: Vec<Value>,
    chunk_bytes: usize,
    details: Vec<String>,
    detail_count: usize,
}

impl<'a, 'db> SectionWriter<'a, 'db> {
    fn new(
        tx: &'a rusqlite::Transaction<'db>,
        thread: &'a str,
        at: &'a str,
        section: &'a str,
        reason: &'a str,
    ) -> Self {
        Self {
            tx,
            thread,
            at,
            section,
            reason,
            items: Vec::new(),
            count: 0,
            chunk: Vec::new(),
            chunk_bytes: 0,
            details: Vec::new(),
            detail_count: 0,
        }
    }

    fn push(&mut self, body: String) -> Result<(), StorageError> {
        self.count += 1;
        let mut value: Value =
            serde_json::from_str(&body).map_err(|e| StorageError::Schema(e.to_string()))?;
        if self.section == "runs"
            && (self.reason != "app_shutdown" || value["stop_reason"].is_null())
        {
            value["stop_reason"] = json!(self.reason);
        }
        if self.reason == "app_shutdown"
            && let Some(state) = value
                .as_object_mut()
                .and_then(|object| object.remove("startup_state"))
        {
            value["planned_state"] = state;
        }
        // Full wait originals already have individually pageable nodes.
        if self.section != "cancelled_waits" {
            self.chunk_bytes = self.chunk_bytes.saturating_add(body.len());
            self.chunk.push(value.clone());
            if self.chunk.len() >= DETAIL_ITEMS || self.chunk_bytes >= DETAIL_INPUT_BYTES {
                self.flush()?;
                self.chunk_bytes = 0;
            }
        }
        if self.items.len() < PREVIEW_ITEMS {
            if let Some(conditions) = value.get_mut("conditions").and_then(Value::as_array_mut) {
                let condition_count = conditions.len();
                conditions.truncate(WAIT_PREVIEW_CONDITIONS);
                value["conditions_count"] = json!(condition_count);
                value["conditions_truncated"] = json!(condition_count > WAIT_PREVIEW_CONDITIONS);
            }
            self.items.push(value);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), StorageError> {
        if self.chunk.is_empty() {
            return Ok(());
        }
        let node = uuid::Uuid::new_v4().to_string();
        let body =
            json!({"section":self.section,"items":std::mem::take(&mut self.chunk)}).to_string();
        crate::history::append_node(
            self.tx,
            self.thread,
            &node,
            None,
            "interruption_details",
            &json!({"text":body}),
            self.at,
        )?;
        self.detail_count += 1;
        if self.details.len() < DETAIL_REFERENCES {
            self.details.push(node);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Value, StorageError> {
        self.flush()?;
        Ok(json!({
            "count":self.count,
            "items":self.items,
            "truncated":self.count>PREVIEW_ITEMS,
            "detail_node_ids":self.details,
            "detail_node_count":self.detail_count,
            "details_kind":"interruption_details",
            "section":self.section,
        }))
    }
}

fn snapshot_section(
    tx: &rusqlite::Transaction<'_>,
    sql: &str,
    thread: &str,
    at: &str,
    section: &str,
    reason: &str,
) -> Result<Value, StorageError> {
    let mut statement = tx.prepare(sql)?;
    let mut query = statement.query([thread])?;
    let mut writer = SectionWriter::new(tx, thread, at, section, reason);
    while let Some(row) = query.next()? {
        writer.push(row.get(0)?)?;
    }
    writer.finish()
}

/// Save the bounded pre-terminal snapshot in the caller's transaction.
/// `reason` and `source` identify lifecycle boundaries, not OS process results.
pub(crate) fn capture_interrupted_context(
    tx: &rusqlite::Transaction<'_>,
    at: &str,
    reason: &'static str,
    source: &'static str,
) -> Result<(), StorageError> {
    let threads = tx
        .prepare(THREADS_WITH_INTERRUPTED_WORK)?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for thread in threads {
        let runs = snapshot_section(tx, RUN_SNAPSHOT, &thread, at, "runs", reason)?;
        let turns = snapshot_section(tx, TURN_SNAPSHOT, &thread, at, "turns", reason)?;
        let shells = snapshot_section(tx, SHELL_SNAPSHOT, &thread, at, "shells", reason)?;
        let tool_calls =
            snapshot_section(tx, TOOL_CALL_SNAPSHOT, &thread, at, "tool_calls", reason)?;
        let children = snapshot_section(
            tx,
            OWNED_CHILDREN_SNAPSHOT,
            &thread,
            at,
            "owned_children",
            reason,
        )?;
        let messages = snapshot_section(
            tx,
            MESSAGE_SNAPSHOT,
            &thread,
            at,
            "unreflected_messages",
            reason,
        )?;
        // Rebuild only the allowed fields; never copy Origin.token or credentials.
        let wait_sql = WAIT_SNAPSHOT;
        let mut references = std::collections::HashMap::new();
        {
            let mut statement = tx.prepare(wait_sql)?;
            let mut query = statement.query([&thread])?;
            while let Some(row) = query.next()? {
                let body: String = row.get(0)?;
                let original: Value =
                    serde_json::from_str(&body).map_err(|e| StorageError::Schema(e.to_string()))?;
                let node = uuid::Uuid::new_v4().to_string();
                crate::history::append_node(
                    tx,
                    &thread,
                    &node,
                    None,
                    "interrupted_wait",
                    &json!({"text":body}),
                    at,
                )?;
                if references.len() < DETAIL_REFERENCES
                    && let Some(operation) = original["operation_id"].as_str()
                {
                    references.insert(operation.to_owned(), node);
                }
            }
        }
        let mut waits = snapshot_section(tx, wait_sql, &thread, at, "cancelled_waits", reason)?;
        waits["history_kind"] = json!("interrupted_wait");
        if let Some(items) = waits["items"].as_array_mut() {
            for item in items {
                if let Some(node) = item["operation_id"]
                    .as_str()
                    .and_then(|id| references.get(id))
                {
                    item["node_id"] = json!(node);
                }
                if let Some(conditions) = item["conditions"].as_array_mut() {
                    conditions.truncate(WAIT_PREVIEW_CONDITIONS);
                }
                item["conditions_are_preview"] = json!(true);
            }
        }
        let unknown = snapshot_section(
            tx,
            PENDING_UNKNOWN_SNAPSHOT,
            &thread,
            at,
            "pending_operations_with_unknown_targets",
            reason,
        )?;
        let actor = json!({"kind":"shellboy","source":source});
        let snapshot_node = uuid::Uuid::new_v4().to_string();
        let payload = json!({"snapshot_node_id":snapshot_node,"kind":reason,"actor":actor,"change":{"reason":reason,"result_uncertain":true,"automatic_replay":false,"os_processes_inspected":false,"runs":runs,"turns":turns,"shells":shells,"tool_calls":tool_calls,"owned_children":children,"unreflected_messages":messages,"cancelled_waits":waits,"pending_operations_with_unknown_targets":unknown},"observed_at":at,"timezone":chrono::Local::now().offset().to_string()});
        crate::history::append_node(
            tx,
            &thread,
            &snapshot_node,
            None,
            "interruption_context",
            &json!({"text":payload.to_string()}),
            at,
        )?;
        tx.execute(
            APPEND_CONTEXT_EVENT,
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                actor.to_string(),
                thread,
                payload.to_string(),
                at
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tool_snapshot_tests {
    use super::*;

    #[test]
    fn terminal_turn_pending_calls_are_recorded_separately_from_actual_shells() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch(r#"
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/','p','m','before');
            INSERT INTO run VALUES('r','t',NULL,'stopped','before','before',NULL);
            INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','stopped','before');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('a','u',0,0,'p','m',0,'completed','{}','before');
            INSERT INTO tool_group VALUES('g','a','u','running','before');
            INSERT INTO tool_call VALUES('g','not-started',0,'shell','{"command":"private command"}','pending',NULL);
            INSERT INTO tool_call VALUES('g','uncertain',1,'shell','{}','running','s');
            INSERT INTO tool_call VALUES('g','saved',2,'shell','{}','completed',NULL);
            INSERT INTO tool_call VALUES('g','missing-execution',3,'shell','{}','running','absent');
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('s','r','u','s','stopped','/','{}','UTF-8','UTF-8','closed','before');
            INSERT INTO node(id,kind,created_at,payload_json) VALUES('result','tool_result','before','{}');
            INSERT INTO tool_result VALUES('g','saved','result','completed');
        "#).unwrap();
        let tx = db.transaction().unwrap();
        capture_interrupted_context(&tx, "after", "previous_instance_interrupted", "startup")
            .unwrap();
        let text:String = tx.query_row("SELECT json_extract(payload_json,'$.text') FROM node WHERE kind='interruption_context'",[],|r|r.get(0)).unwrap();
        let snapshot: Value = serde_json::from_str(&text).unwrap();
        let change = &snapshot["change"];
        assert_eq!(change["runs"]["count"], 0);
        assert_eq!(
            change["shells"]["count"], 0,
            "stopped execution is not an active OS shell"
        );
        assert_eq!(change["tool_calls"]["count"], 4);
        let items = change["tool_calls"]["items"].as_array().unwrap();
        assert_eq!(items[0]["repair_outcome"], "not_started");
        assert!(items[0]["execution_id"].is_null());
        assert_eq!(items[1]["repair_outcome"], "result_uncertain");
        assert_eq!(items[1]["execution_id"], "s");
        assert_eq!(items[2]["repair_outcome"], "preserved_result");
        assert_eq!(items[2]["result_saved"], true);
        assert_eq!(items[3]["repair_outcome"], "result_uncertain");
        assert!(items[3]["execution_id"].is_null());
        assert!(!text.contains("private command"));
        assert_eq!(
            tx.query_row("SELECT state FROM tool_group WHERE id='g'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "running",
            "capture must not mutate repair state"
        );
        tx.rollback().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM node WHERE kind='interruption_context'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
