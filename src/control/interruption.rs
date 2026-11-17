//! Bounded pre-terminal context captured by startup and shutdown transactions.
use crate::storage::StorageError;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

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
        SELECT 1
        FROM tool_group_recovery_candidate rc
        CROSS JOIN tool_group g ON g.id=rc.group_id
        JOIN turn u ON u.id=g.turn_id
        WHERE u.thread_id=t.id)
    OR EXISTS (
        SELECT 1
        FROM event e
        WHERE e.target_thread_id = t.id
        AND e.kind = 'context_changed'
        AND json_extract(e.payload_ref_json, '$.kind') = 'interruption_capture_header')
    OR EXISTS (
        SELECT 1
        FROM app_meta m
        WHERE m.key LIKE 'wait.pending.%'
        AND (json_extract(m.value, '$.caller.thread_id') = t.id
            OR (json_extract(m.value, '$.caller') IS NULL
                AND EXISTS (
                    SELECT 1
                    FROM json_each(m.value, '$.conditions') c
                    WHERE EXISTS(SELECT 1 FROM json_each(c.value,'$.thread_ids') ids WHERE ids.value=t.id) OR json_extract(c.value, '$.run_id') IN (
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
    'end_message_node_id', u.answer_node_id,
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

const SHELL_SNAPSHOT: &str = r"SELECT json_patch(json_object('execution_id', s.id,
    'run_id', s.owner_run_id,
    'turn_id', s.turn_id,
    'previous_state', s.state,
    'startup_state', ?2,
    'output_complete', json('false'),
    'saved_sequence', s.saved_sequence,
    'retention_generation', s.retention_generation,
    'result_uncertain', json('true')), json(?3))
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
-- Start from the bounded candidate set; CROSS JOIN prevents SQLite from
-- choosing the completed call history as the outer loop.
FROM tool_group_recovery_candidate rc
CROSS JOIN tool_group g ON g.id=rc.group_id
CROSS JOIN turn u ON u.id=g.turn_id
CROSS JOIN tool_call c ON c.group_id=g.id
LEFT JOIN tool_result z ON z.group_id=c.group_id AND z.call_id=c.call_id
LEFT JOIN shell_execution s ON s.id=c.operation_id AND s.owner_run_id=u.run_id
WHERE u.thread_id=?1 AND (g.state<>'completed' OR EXISTS (
    SELECT 1 FROM tool_call pending WHERE pending.group_id=g.id
    AND (pending.state IN ('pending','running') OR NOT EXISTS (
        SELECT 1 FROM tool_result saved WHERE saved.group_id=pending.group_id AND saved.call_id=pending.call_id))))
ORDER BY g.created_at,g.id,c.ordinal";

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
    'after_cursor', json_extract(m.value, '$.after_cursor'),
    'accepted_at', json_extract(m.value, '$.accepted_at'),
    'observation_state', 'interrupted')
FROM app_meta m
WHERE m.key LIKE 'wait.pending.%'
AND (json_extract(m.value, '$.caller.thread_id') = ?1
    OR (json_extract(m.value, '$.caller') IS NULL
        AND EXISTS (
            SELECT 1
            FROM json_each(m.value, '$.conditions') c
            WHERE EXISTS(SELECT 1 FROM json_each(c.value,'$.thread_ids') ids WHERE ids.value=?1) OR json_extract(c.value, '$.run_id') IN (
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
const APPEND_CAPTURE_EVENT: &str = r"INSERT INTO event(id, kind, source_json, target_thread_id, payload_ref_json, created_at) VALUES(?1, 'context_changed', ?2, ?3, ?4, ?5)";

struct CaptureStager<'a, 'db> {
    tx: &'a rusqlite::Transaction<'db>,
    thread: &'a str,
    at: &'a str,
    capture_id: String,
    actor_json: String,
    ordinal: u64,
    final_event_id: String,
}

impl<'a, 'db> CaptureStager<'a, 'db> {
    fn new(tx: &'a rusqlite::Transaction<'db>, thread: &'a str, at: &'a str, source: &str) -> Self {
        Self {
            tx,
            thread,
            at,
            capture_id: uuid::Uuid::new_v4().to_string(),
            actor_json: json!({"kind":"shellboy","source":source}).to_string(),
            ordinal: 0,
            final_event_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    fn stage_node(&mut self, kind: &str, payload: &Value) -> Result<String, StorageError> {
        let node_id = uuid::Uuid::new_v4().to_string();
        self.stage_node_with_id(kind, payload, &node_id)?;
        Ok(node_id)
    }

    fn stage_node_with_id(
        &mut self,
        kind: &str,
        payload: &Value,
        node_id: &str,
    ) -> Result<(), StorageError> {
        let payload_json = payload.to_string();
        let body_hash = crate::digest::sha256_hex(payload_json.as_bytes());
        let event_id = format!("ic:{}:{:020}", self.capture_id, self.ordinal);
        let row = json!({
            "kind":"interruption_capture_row",
            "capture_id":self.capture_id,
            "ordinal":self.ordinal,
            "node":{"id":node_id,"kind":kind,"turn_id":Value::Null,
                "created_at":self.at,"payload_json":payload_json,
                "payload_bytes":payload_json.len(),"body_hash":body_hash}
        });
        self.tx.execute(
            APPEND_CAPTURE_EVENT,
            rusqlite::params![
                event_id,
                self.actor_json,
                self.thread,
                row.to_string(),
                self.at
            ],
        )?;
        self.ordinal = self
            .ordinal
            .checked_add(1)
            .ok_or_else(|| StorageError::Schema("interruption capture ordinal overflow".into()))?;
        Ok(())
    }

    fn finish(self, final_payload: &Value) -> Result<(), StorageError> {
        let header_id = format!("ic:{}:header", self.capture_id);
        let header = json!({
            "kind":"interruption_capture_header",
            "capture_id":self.capture_id,
            "row_count":self.ordinal,
            "final_event_id":self.final_event_id,
            "final_payload":final_payload,
            "reason":final_payload["change"]["reason"],
            "source":final_payload["actor"]["source"],
        });
        self.tx.execute(
            APPEND_CAPTURE_EVENT,
            rusqlite::params![
                header_id,
                self.actor_json,
                self.thread,
                header.to_string(),
                self.at
            ],
        )?;
        Ok(())
    }
}

/// Owns each section's bounded preview and independently pageable originals.
struct SectionWriter<'a, 'tx, 'db> {
    stager: &'a mut CaptureStager<'tx, 'db>,
    section: &'a str,
    reason: &'a str,
    items: Vec<Value>,
    count: usize,
    chunk: Vec<Value>,
    chunk_bytes: usize,
    details: Vec<String>,
    detail_count: usize,
}

impl<'a, 'tx, 'db> SectionWriter<'a, 'tx, 'db> {
    fn new(stager: &'a mut CaptureStager<'tx, 'db>, section: &'a str, reason: &'a str) -> Self {
        Self {
            stager,
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
        if self.section == "runs" && value["stop_reason"].is_null() {
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
        let body =
            json!({"section":self.section,"items":std::mem::take(&mut self.chunk)}).to_string();
        let node = self
            .stager
            .stage_node("interruption_details", &json!({"text":body}))?;
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
    section: &str,
    reason: &str,
    stager: &mut CaptureStager<'_, '_>,
) -> Result<Value, StorageError> {
    snapshot_section_with_params(tx, sql, [thread], section, reason, stager)
}

fn snapshot_section_with_params(
    tx: &rusqlite::Transaction<'_>,
    sql: &str,
    params: impl rusqlite::Params,
    section: &str,
    reason: &str,
    stager: &mut CaptureStager<'_, '_>,
) -> Result<Value, StorageError> {
    let mut statement = tx.prepare(sql)?;
    let mut query = statement.query(params)?;
    let mut writer = SectionWriter::new(stager, section, reason);
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
    capture_context(tx, at, reason, source, false).map(|_| ())
}

pub(crate) fn capture_startup_interrupted_context(
    tx: &rusqlite::Transaction<'_>,
    at: &str,
) -> Result<Vec<crate::diagnostics::ToolRepairFailure>, StorageError> {
    let root_exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key='root_revision')",
        [],
        |r| r.get(0),
    )?;
    if !root_exists {
        let mut error = crate::protocol::ApiError::new(
            crate::protocol::ErrorCode::SaveFailed,
            "Global root revision metadata is missing at startup capture",
        );
        error.details = Some(json!({"stage":"startup_global_metadata"}));
        return Err(error.into());
    }
    capture_context(tx, at, "previous_instance_interrupted", "startup", true)
}

fn capture_context(
    tx: &rusqlite::Transaction<'_>,
    at: &str,
    reason: &'static str,
    source: &'static str,
    isolate_bad_cursor: bool,
) -> Result<Vec<crate::diagnostics::ToolRepairFailure>, StorageError> {
    let mut failures = Vec::new();
    let threads = tx
        .prepare(THREADS_WITH_INTERRUPTED_WORK)?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for thread in threads {
        if isolate_bad_cursor {
            let valid: bool = tx.query_row("SELECT cursor_node_id IS (SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal DESC LIMIT 1) FROM thread WHERE id=?1",[&thread],|r|r.get(0))?;
            if !valid {
                let reason = crate::diagnostics::ToolRepairReason::CaptureCursorMismatch;
                if failures.len() < 32 {
                    failures.push(crate::diagnostics::ToolRepairFailure {
                        thread_id: thread.chars().take(128).collect(),
                        group_id: None,
                        reason,
                    });
                }
                continue;
            }
        }
        let already_staged: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE target_thread_id=?1 AND kind='context_changed' AND json_extract(payload_ref_json,'$.kind')='interruption_capture_header')",
            [&thread],
            |row| row.get(0),
        )?;
        if already_staged {
            continue;
        }
        let captured = (|| -> Result<(), StorageError> {
            let mut stager = CaptureStager::new(tx, &thread, at, source);
            let runs = snapshot_section(tx, RUN_SNAPSHOT, &thread, "runs", reason, &mut stager)?;
            let turns = snapshot_section(tx, TURN_SNAPSHOT, &thread, "turns", reason, &mut stager)?;
            let unverified = cfg!(unix) && reason == "previous_instance_interrupted";
            let shells = snapshot_section_with_params(
                tx,
                SHELL_SNAPSHOT,
                rusqlite::params![
                    &thread,
                    if unverified { "failed" } else { "stopped" },
                    if unverified {
                        r#"{"os_state":"unverified"}"#
                    } else {
                        "{}"
                    },
                ],
                "shells",
                reason,
                &mut stager,
            )?;
            let tool_calls = snapshot_section(
                tx,
                TOOL_CALL_SNAPSHOT,
                &thread,
                "tool_calls",
                reason,
                &mut stager,
            )?;
            let messages = snapshot_section(
                tx,
                MESSAGE_SNAPSHOT,
                &thread,
                "unreflected_messages",
                reason,
                &mut stager,
            )?;
            // Rebuild only the allowed fields; never copy Origin.token or credentials.
            let wait_sql = WAIT_SNAPSHOT;
            let mut references = std::collections::HashMap::new();
            {
                let mut statement = tx.prepare(wait_sql)?;
                let mut query = statement.query([&thread])?;
                while let Some(row) = query.next()? {
                    let body: String = row.get(0)?;
                    let original: Value = serde_json::from_str(&body)
                        .map_err(|e| StorageError::Schema(e.to_string()))?;
                    let node = stager.stage_node("interrupted_wait", &json!({"text":body}))?;
                    if references.len() < DETAIL_REFERENCES
                        && let Some(operation) = original["operation_id"].as_str()
                    {
                        references.insert(operation.to_owned(), node);
                    }
                }
            }
            let mut waits = snapshot_section(
                tx,
                wait_sql,
                &thread,
                "cancelled_waits",
                reason,
                &mut stager,
            )?;
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
                "pending_operations_with_unknown_targets",
                reason,
                &mut stager,
            )?;
            let actor = json!({"kind":"shellboy","source":source});
            let snapshot_node = uuid::Uuid::new_v4().to_string();
            let payload = json!({"snapshot_node_id":snapshot_node,"kind":reason,"actor":actor,"change":{"reason":reason,"result_uncertain":true,"automatic_replay":false,"os_processes_inspected":false,"runs":runs,"turns":turns,"shells":shells,"tool_calls":tool_calls,"unreflected_messages":messages,"cancelled_waits":waits,"pending_operations_with_unknown_targets":unknown},"observed_at":at,"timezone":chrono::Local::now().offset().to_string()});
            stager.stage_node_with_id(
                "interruption_context",
                &json!({"text":payload.to_string()}),
                &snapshot_node,
            )?;
            stager.finish(&payload)?;
            Ok(())
        })();
        if let Err(error) = captured {
            let mut error = error.into_api_error();
            error.message = format!(
                "Interrupted context capture failed for Thread {}: {}",
                thread.chars().take(128).collect::<String>(),
                error.message
            );
            error.details = Some(json!({"thread_id":thread.chars().take(128).collect::<String>()}));
            return Err(error.into());
        }
    }
    Ok(failures)
}

const CAPTURE_PAGE: usize = 64;

pub(crate) fn staged_capture_threads(db: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = db.prepare("SELECT DISTINCT target_thread_id FROM event WHERE kind='context_changed' AND json_extract(payload_ref_json,'$.kind')='interruption_capture_header' ORDER BY target_thread_id")?;
    statement.query_map([], |row| row.get(0))?.collect()
}

/// Consume a complete saved capture atomically for this Thread after unwind.
pub(crate) fn consume_staged_captures(
    tx: &Transaction<'_>,
    thread: &str,
    at: &str,
) -> Result<(), StorageError> {
    let headers: Vec<(String, String, String, String)> = tx
        .prepare("SELECT id,source_json,payload_ref_json,created_at FROM event WHERE target_thread_id=?1 AND kind='context_changed' AND json_extract(payload_ref_json,'$.kind')='interruption_capture_header' ORDER BY created_at,id")?
        .query_map([thread], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;

    for (header_id, source_json, header_json, created_at) in headers {
        let header: Value = serde_json::from_str(&header_json)
            .map_err(|error| StorageError::Schema(error.to_string()))?;
        let capture_id = header["capture_id"]
            .as_str()
            .ok_or_else(|| StorageError::Schema("capture header has no identity".into()))?;
        let row_count = header["row_count"]
            .as_u64()
            .ok_or_else(|| StorageError::Schema("capture header has no row count".into()))?;
        let final_event_id = header["final_event_id"]
            .as_str()
            .ok_or_else(|| StorageError::Schema("capture header has no final event ID".into()))?;
        let final_payload = header
            .get("final_payload")
            .filter(|payload| payload.is_object())
            .ok_or_else(|| StorageError::Schema("capture header has no final payload".into()))?;
        let final_node_id = final_payload["snapshot_node_id"].as_str().ok_or_else(|| {
            StorageError::Schema("capture final payload has no snapshot node".into())
        })?;
        let reason = header["reason"]
            .as_str()
            .ok_or_else(|| StorageError::Schema("capture header has no reason".into()))?;
        let source = header["source"]
            .as_str()
            .ok_or_else(|| StorageError::Schema("capture header has no source".into()))?;
        let source_value: Value = serde_json::from_str(&source_json)
            .map_err(|error| StorageError::Schema(error.to_string()))?;
        if header["kind"] != "interruption_capture_header"
            || final_payload["kind"] != reason
            || final_payload["actor"]["source"] != source
            || source_value["kind"] != "shellboy"
            || source_value["source"] != source
        {
            return Err(StorageError::Schema(
                "capture header provenance mismatch".into(),
            ));
        }
        let target_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
            [thread],
            |row| row.get(0),
        )?;
        if !target_exists {
            return Err(StorageError::Schema(
                "capture target Thread is unavailable".into(),
            ));
        }

        let prefix = format!("ic:{capture_id}:");
        let upper = format!("ic:{capture_id};");
        let expected_final_text = final_payload.to_string();
        let mut after = prefix.clone();
        let mut ordinal = 0_u64;
        let mut saw_final_node = false;
        loop {
            let page: Vec<(String, String, String)> = tx
                .prepare("SELECT id,payload_ref_json,created_at FROM event WHERE target_thread_id=?1 AND kind='context_changed' AND created_at=?2 AND id>?3 AND id<?4 AND json_extract(payload_ref_json,'$.kind')='interruption_capture_row' AND json_extract(payload_ref_json,'$.capture_id')=?5 ORDER BY id LIMIT ?6")?
                .query_map(params![thread,created_at,after,upper,capture_id,CAPTURE_PAGE as i64], |row| {
                    Ok((row.get(0)?,row.get(1)?,row.get(2)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            if page.is_empty() {
                break;
            }
            let mut node_ids = Vec::with_capacity(page.len());
            for (event_id, row_json, row_created_at) in &page {
                let row: Value = serde_json::from_str(row_json)
                    .map_err(|error| StorageError::Schema(error.to_string()))?;
                if row["kind"] != "interruption_capture_row"
                    || row["capture_id"] != capture_id
                    || row["ordinal"].as_u64() != Some(ordinal)
                    || row_created_at != &created_at
                {
                    return Err(StorageError::Schema(
                        "capture row order or provenance mismatch".into(),
                    ));
                }
                let node = &row["node"];
                let node_id = node["id"]
                    .as_str()
                    .ok_or_else(|| StorageError::Schema("capture row has no node ID".into()))?;
                let kind = node["kind"]
                    .as_str()
                    .filter(|kind| {
                        matches!(
                            *kind,
                            "interruption_details" | "interrupted_wait" | "interruption_context"
                        )
                    })
                    .ok_or_else(|| {
                        StorageError::Schema("capture row has invalid node kind".into())
                    })?;
                let node_created_at = node["created_at"]
                    .as_str()
                    .filter(|created| *created == created_at)
                    .ok_or_else(|| {
                        StorageError::Schema("capture node timestamp mismatch".into())
                    })?;
                let payload_json = node["payload_json"].as_str().ok_or_else(|| {
                    StorageError::Schema("capture node payload is missing".into())
                })?;
                let payload: Value = serde_json::from_str(payload_json)
                    .map_err(|error| StorageError::Schema(error.to_string()))?;
                let payload_bytes = i64::try_from(payload_json.len()).map_err(|_| {
                    StorageError::Schema("capture node payload is too large".into())
                })?;
                let body_hash = crate::digest::sha256_hex(payload_json.as_bytes());
                if node["turn_id"] != Value::Null
                    || node["payload_bytes"].as_i64() != Some(payload_bytes)
                    || node["body_hash"].as_str() != Some(body_hash.as_str())
                {
                    return Err(StorageError::Schema(
                        "capture node content witness mismatch".into(),
                    ));
                }
                if ordinal == 0 && event_id != &format!("{prefix}{ordinal:020}") {
                    return Err(StorageError::Schema(
                        "capture row key does not match ordinal".into(),
                    ));
                }
                if event_id != &format!("{prefix}{ordinal:020}") {
                    return Err(StorageError::Schema(
                        "capture row key does not match ordinal".into(),
                    ));
                }
                if node_id == final_node_id {
                    saw_final_node = ordinal + 1 == row_count
                        && kind == "interruption_context"
                        && payload["text"].as_str() == Some(expected_final_text.as_str());
                }
                let existing: Option<(Option<String>,String,String,Option<String>,i64,Option<String>,Option<String>)> = tx
                    .query_row("SELECT turn_id,kind,created_at,payload_json,payload_bytes,body_hash,deleted_reason FROM node WHERE id=?1",[node_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)))
                    .optional()?;
                match existing {
                    Some((
                        turn,
                        existing_kind,
                        existing_at,
                        existing_payload,
                        existing_bytes,
                        existing_hash,
                        deleted,
                    )) if turn.is_none()
                        && existing_kind == kind
                        && existing_at == node_created_at
                        && existing_payload.as_deref() == Some(payload_json)
                        && existing_bytes == payload_bytes
                        && existing_hash.as_deref() == Some(body_hash.as_str())
                        && deleted.is_none() => {}
                    Some(_) => {
                        return Err(StorageError::Schema(
                            "capture node ID conflicts with existing content".into(),
                        ));
                    }
                    None => {
                        tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES(?1,NULL,?2,?3,?4,?5,?6)",params![node_id,kind,node_created_at,payload_json,payload_bytes,body_hash])?;
                    }
                }
                node_ids.push(node_id.to_owned());
                ordinal = ordinal
                    .checked_add(1)
                    .ok_or_else(|| StorageError::Schema("capture row count overflow".into()))?;
                after = event_id.clone();
            }
            let revision =
                crate::history::selection_revision(tx, thread, crate::history::ThreadScope::Live)?;
            crate::history::append_existing_nodes(tx, thread, revision as u64, &node_ids, at)?;
        }
        if ordinal != row_count || !saw_final_node {
            return Err(StorageError::Schema(
                "capture is incomplete or final node is missing".into(),
            ));
        }

        let final_payload_json = final_payload.to_string();
        let existing_event: Option<(String, String, String)> = tx
            .query_row(
                "SELECT source_json,target_thread_id,payload_ref_json FROM event WHERE id=?1",
                [final_event_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        match existing_event {
            Some((existing_source, existing_thread, existing_payload))
                if existing_source == source_json
                    && existing_thread == thread
                    && existing_payload == final_payload_json => {}
            Some(_) => {
                return Err(StorageError::Schema(
                    "capture final event ID conflicts with existing content".into(),
                ));
            }
            None => {
                tx.execute(
                    APPEND_CONTEXT_EVENT,
                    params![
                        final_event_id,
                        source_json,
                        thread,
                        final_payload_json,
                        created_at
                    ],
                )?;
            }
        }
        let deleted_rows = tx.execute(
            "DELETE FROM event WHERE target_thread_id=?1 AND kind='context_changed' AND created_at=?2 AND id>=?3 AND id<?4 AND json_extract(payload_ref_json,'$.kind')='interruption_capture_row' AND json_extract(payload_ref_json,'$.capture_id')=?5",
            params![thread,created_at,prefix,upper,capture_id],
        )?;
        if u64::try_from(deleted_rows).ok() != Some(row_count) {
            return Err(StorageError::Schema(
                "capture row deletion count mismatch".into(),
            ));
        }
        if tx.execute(
            "DELETE FROM event WHERE id=?1 AND target_thread_id=?2 AND kind='context_changed' AND json_extract(payload_ref_json,'$.kind')='interruption_capture_header' AND json_extract(payload_ref_json,'$.capture_id')=?3",
            params![header_id,thread,capture_id],
        )? != 1 {
            return Err(StorageError::Schema("capture header disappeared before consume".into()));
        }
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
            INSERT INTO run(id,thread_id,state,created_at,ended_at,stop_reason) VALUES('r','t','stopped','before','before',NULL);
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
            INSERT INTO tool_group VALUES('done','a','u','completed','before');
            INSERT INTO tool_call VALUES('done','finished',0,'shell','{}','completed',NULL);
            INSERT INTO node(id,kind,created_at,payload_json) VALUES('done-result','tool_result','before','{}');
            INSERT INTO tool_result VALUES('done','finished','done-result','completed');
            WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<256)
                INSERT INTO tool_group SELECT printf('bulk-%03d',x),'a','u','completed','before' FROM n;
            INSERT INTO tool_call SELECT id,'finished',0,'shell','{}','completed',NULL FROM tool_group WHERE id GLOB 'bulk-*';
            INSERT INTO node(id,kind,created_at,payload_json) SELECT 'bulk-result-'||id,'tool_result','before','{}' FROM tool_group WHERE id GLOB 'bulk-*';
            INSERT INTO tool_result SELECT id,'finished','bulk-result-'||id,'completed' FROM tool_group WHERE id GLOB 'bulk-*';
        "#).unwrap();
        let legacy_candidates: Vec<String> = db
            .prepare("SELECT g.id FROM tool_group g WHERE g.state<>'completed' OR EXISTS (SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state IN ('pending','running') OR NOT EXISTS (SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id))) ORDER BY g.id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let marked_candidates: Vec<String> = db
            .prepare("SELECT group_id FROM tool_group_recovery_candidate ORDER BY group_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(marked_candidates, legacy_candidates);
        let selected_threads: Vec<String> = db
            .prepare(THREADS_WITH_INTERRUPTED_WORK)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(selected_threads, ["t"]);
        let tx = db.transaction().unwrap();
        capture_interrupted_context(&tx, "after", "previous_instance_interrupted", "startup")
            .unwrap();
        assert_eq!(
            tx.query_row(
                "SELECT count(*) FROM node WHERE kind='interruption_context'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert!(tx.query_row("SELECT count(*) FROM event WHERE json_extract(payload_ref_json,'$.kind')='interruption_capture_row'",[],|r|r.get::<_,i64>(0)).unwrap()>0);
        assert_eq!(
            tx.query_row(
                "SELECT selection_revision FROM thread WHERE id='t'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        consume_staged_captures(&tx, "t", "after-consume").unwrap();
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
            db.query_row("SELECT count(*) FROM event WHERE json_extract(payload_ref_json,'$.kind') LIKE 'interruption_capture_%'", [], |r| r.get::<_, i64>(0)).unwrap(),
            0,
            "capture staging and selected-path adoption roll back together"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM node WHERE kind='interruption_context'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        db.execute("DELETE FROM app_meta WHERE key='root_revision'", [])
            .unwrap();
        let tx = db.transaction().unwrap();
        let error = capture_startup_interrupted_context(&tx, "fatal-startup")
            .unwrap_err()
            .into_api_error();
        assert_eq!(error.code, crate::protocol::ErrorCode::SaveFailed);
        let details = error.details.unwrap();
        assert_eq!(details["stage"], "startup_global_metadata");
        tx.rollback().unwrap();
        db.execute("UPDATE thread SET cursor_node_id='result' WHERE id='t'", [])
            .unwrap();
        let tx = db.transaction().unwrap();
        assert_eq!(
            capture_startup_interrupted_context(&tx, "all-skipped-without-global-root")
                .unwrap_err()
                .into_api_error()
                .code,
            crate::protocol::ErrorCode::SaveFailed
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

#[cfg(test)]
mod stop_reason_review_tests {
    use super::*;
    #[test]
    fn startup_and_shutdown_capture_preserve_explicit_stop_reason() {
        for reason in ["previous_instance_interrupted", "app_shutdown"] {
            let mut db = rusqlite::Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/','p','m','now'); INSERT INTO run(id,thread_id,state,created_at,stop_reason)VALUES('r','t','stopping','now','explicit_stop');").unwrap();
            let tx = db.transaction().unwrap();
            capture_interrupted_context(&tx, "later", reason, "test").unwrap();
            consume_staged_captures(&tx, "t", "later-consume").unwrap();
            let text:String=tx.query_row("SELECT json_extract(payload_json,'$.text') FROM node WHERE kind='interruption_context'",[],|r|r.get(0)).unwrap();
            let snapshot: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                snapshot["change"]["runs"]["items"][0]["stop_reason"],
                "explicit_stop"
            );
            assert_eq!(
                tx.query_row("SELECT stop_reason FROM run WHERE id='r'", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                "explicit_stop"
            );
        }
    }
}
