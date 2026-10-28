//! Startup-only schema upgrades. Runtime readers use the current schema only.
use super::{StorageError, validate_schema};
use rusqlite::Connection;

const HOT_QUERY_INDEX_MIGRATION: &str = include_str!("../../migrations/0010_hot_query_indexes.sql");
const GC_INDEX_MIGRATION: &str = include_str!("../../migrations/0002_gc_indexes.sql");
const ATTACHMENT_PROVENANCE_MIGRATION: &str =
    include_str!("../../migrations/0004_attachment_provenance.sql");
const ORIGIN_CREDENTIAL_MIGRATION: &str =
    include_str!("../../migrations/0005_origin_credentials.sql");
const RECEIPT_BUDGET_MIGRATION: &str = include_str!("../../migrations/0006_receipt_budget.sql");
const USAGE_SOURCE_INDEX_MIGRATION: &str =
    include_str!("../../migrations/0007_usage_and_source_indexes.sql");
const RECEIPT_ACCOUNTING_COMPAT_MIGRATION: &str =
    include_str!("../../migrations/0008_receipt_accounting_compat.sql");
const COMPACTION_TURN_INDEX_MIGRATION: &str =
    include_str!("../../migrations/0009_compaction_turn_indexes.sql");
const CONTEXT_INDEX_MIGRATION: &str = include_str!("../../migrations/0003_context_indexes.sql");

pub(super) fn run(connection: &mut Connection, version: i64) -> Result<(), StorageError> {
    if version < 2 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(GC_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 3 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(CONTEXT_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 4 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(ATTACHMENT_PROVENANCE_MIGRATION)?;
        migrate_attachment_sources(&transaction)?;
        transaction.commit()?;
    }
    if version < 5 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(ORIGIN_CREDENTIAL_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 6 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(RECEIPT_BUDGET_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 7 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(USAGE_SOURCE_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 8 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(RECEIPT_ACCOUNTING_COMPAT_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 9 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(COMPACTION_TURN_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 10 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(HOT_QUERY_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 11 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(include_str!(
            "../../migrations/0011_current_index_metadata.sql"
        ))?;
        transaction.commit()?;
    }
    validate_schema(connection)?;
    Ok(())
}

/// Preserve known user provenance; unknown legacy origins stay pending and are
/// never silently attached to the newest human message.
fn migrate_attachment_sources(tx: &rusqlite::Transaction<'_>) -> Result<(), StorageError> {
    use rusqlite::params;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    let rows=tx.prepare("SELECT a.id,a.source_node_id,n.kind,n.payload_json FROM attachment_ref a LEFT JOIN node n ON n.id=a.source_node_id ORDER BY a.created_at,a.id")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (attachment, source, kind, raw) in rows {
        let selections=tx.prepare("SELECT thread_id,reason FROM attachment_selection WHERE attachment_id=?1 ORDER BY thread_id")?.query_map([&attachment],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
        // Compaction already retired these source slots. Preserve their
        // tombstones instead of manufacturing a pending source for replay.
        if source.is_some()
            && kind.is_some()
            && raw.is_none()
            && selections
                .iter()
                .all(|(_, reason)| reason.as_deref() == Some("context_replacement"))
        {
            continue;
        }
        let mut payload: Value = raw
            .as_deref()
            .and_then(|r| serde_json::from_str(r).ok())
            .unwrap_or(Value::Null);
        let known = payload.is_object()
            && (matches!(kind.as_deref(), Some("user") | Some("attachment"))
                || (kind.as_deref() == Some("event")
                    && payload.get("role").and_then(Value::as_str) == Some("user")
                    && payload.get("content").and_then(Value::as_array).is_some()));
        let source = if known {
            source.expect("joined source")
        } else {
            let previous_source = source;
            let source = format!("legacy-attachment-{attachment}");
            let payload=json!({"role":"user","content":[{"type":"input_text","text":"[Shellboy legacy attachment recovery] Original provenance or historical position could not be recovered. This auxiliary attachment source is not a new human message."},{"type":"attachment_ref","attachment_id":attachment}],"attachment_operation":{"origin":"legacy_unknown","previous_source_node_id":previous_source,"provenance_uncertain":true}}).to_string();
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,'attachment',?2,?3,?4,?5)",params![source,chrono::Utc::now().to_rfc3339(),payload,payload.len() as i64,Sha256::digest(payload.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>()])?;
            tx.execute(
                "UPDATE attachment_ref SET source_node_id=?2 WHERE id=?1",
                params![attachment, source],
            )?;
            // Forks share one immutable source but adopt it independently.
            for (thread, reason) in &selections {
                let status = if reason.as_deref() == Some("context_replacement") {
                    "cancelled"
                } else {
                    "accepted"
                };
                tx.execute("INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at)VALUES(?1,?2,?3,?4,?5,?6)",params![source,thread,status,format!("migration:{attachment}"),json!({"origin":"legacy_unknown","provenance_uncertain":true}).to_string(),chrono::Utc::now().to_rfc3339()])?;
            }
            continue;
        };
        if let Some(text) = payload.get("content").and_then(Value::as_str) {
            payload["content"] = json!([{"type":"input_text","text":text}]);
        }
        if payload.get("content").and_then(Value::as_array).is_none() {
            payload["content"] = json!([]);
        }
        let content = payload["content"]
            .as_array_mut()
            .expect("normalized content");
        if !content.iter().any(|v| {
            v.get("type").and_then(Value::as_str) == Some("attachment_ref")
                && v.get("attachment_id").and_then(Value::as_str) == Some(attachment.as_str())
        }) {
            content.push(json!({"type":"attachment_ref","attachment_id":attachment}));
        }
        let payload = payload.to_string();
        tx.execute("UPDATE node SET payload_json=?2,payload_bytes=?3,body_hash=?4,payload_revision=payload_revision+1 WHERE id=?1",params![source,payload,payload.len() as i64,Sha256::digest(payload.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>()])?;
    }
    Ok(())
}
