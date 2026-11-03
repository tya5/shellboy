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

pub(super) fn run(
    connection: &mut Connection,
    version: i64,
    report: &dyn Fn(i64, i64),
    validation: &dyn Fn(),
) -> Result<(), StorageError> {
    if version < 2 {
        report(2 - version, super::SCHEMA_VERSION - version);
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(GC_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 3 {
        report(3 - version, super::SCHEMA_VERSION - version);
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(CONTEXT_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 4 {
        report(4 - version, super::SCHEMA_VERSION - version);
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(ATTACHMENT_PROVENANCE_MIGRATION)?;
        migrate_attachment_sources(&transaction)?;
        transaction.commit()?;
    }
    if version < 5 {
        report(5 - version, super::SCHEMA_VERSION - version);
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(ORIGIN_CREDENTIAL_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 6 {
        report(6 - version, super::SCHEMA_VERSION - version);
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(RECEIPT_BUDGET_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 7 {
        report(7 - version, super::SCHEMA_VERSION - version);
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(USAGE_SOURCE_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 8 {
        report(8 - version, super::SCHEMA_VERSION - version);
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(RECEIPT_ACCOUNTING_COMPAT_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 9 {
        report(9 - version, super::SCHEMA_VERSION - version);
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(COMPACTION_TURN_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 10 {
        report(10 - version, super::SCHEMA_VERSION - version);
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(HOT_QUERY_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 11 {
        report(11 - version, super::SCHEMA_VERSION - version);
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(include_str!(
            "../../migrations/0011_current_index_metadata.sql"
        ))?;
        transaction.commit()?;
    }
    if version < 12 {
        report(12 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/0012_revisit_metadata.sql"))?;
        tx.commit()?;
    }
    if version < 13 {
        report(13 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!(
            "../../migrations/0013_message_path_admission.sql"
        ))?;
        tx.commit()?;
    }
    if version < 14 {
        report(14 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/0014_turn_interrupt.sql"))?;
        tx.commit()?;
    }
    if version < 15 {
        report(15 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/0015_readback_indexes.sql"))?;
        tx.commit()?;
    }
    if version < 16 {
        report(16 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/0016_thread_observation.sql"))?;
        tx.commit()?;
    }
    if version < 17 {
        report(17 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/0017_thread_parent.sql"))?;
        tx.commit()?;
    }
    if version < 18 {
        report(18 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!(
            "../../migrations/0018_summary_usage_metadata.sql"
        ))?;
        tx.commit()?;
    }
    if version < 19 {
        report(19 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!(
            "../../migrations/0019_context_attempt_scope.sql"
        ))?;
        tx.commit()?;
    }
    if version < 20 {
        report(20 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/0020_gc_index_source.sql"))?;
        tx.commit()?;
    }
    if version < 21 {
        report(21 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/0021_event_retention.sql"))?;
        tx.commit()?;
    }
    if version < 22 {
        report(22 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!(
            "../../migrations/0022_logical_usage_counters.sql"
        ))?;
        tx.commit()?;
    }
    if version < 23 {
        report(23 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/0023_activity_bounded.sql"))?;
        tx.commit()?;
    }
    if version < 24 {
        report(24 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/0024_revisit_messages.sql"))?;
        tx.commit()?;
    }
    if version < 25 {
        report(25 - version, super::SCHEMA_VERSION - version);
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/0025_index_recovery.sql"))?;
        tx.commit()?;
    }
    validation();
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

#[cfg(test)]
mod activity_migration_tests {
    use super::*;

    #[test]
    fn version_22_upgrade_preserves_rows_and_installs_activity_indexes() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("DROP INDEX activity_attempt_turn; DROP INDEX activity_turn_run; DROP INDEX activity_turn_run_state; DROP INDEX activity_group_turn_state; DROP INDEX activity_shell_spawned_turn; DROP INDEX activity_shell_uncollected_owner; DROP INDEX receipt_run_completion; DELETE FROM schema_migration WHERE version>=23; PRAGMA user_version=22; INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now');").unwrap();
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','now'); PRAGMA ignore_check_constraints=ON; INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('bad','r','u','bad','exited','/','broken json','UTF-8','UTF-8','closed','now'),('good','r','u','good','exited','/','{\"spawned\":true}','UTF-8','UTF-8','closed','now'); PRAGMA ignore_check_constraints=OFF;").unwrap();
        let observed = std::cell::RefCell::new(Vec::new());
        run(
            &mut db,
            22,
            &|current, total| observed.borrow_mut().push((current, total)),
            &|| {},
        )
        .unwrap();
        assert_eq!(*observed.borrow(), vec![(1, 3), (2, 3), (3, 3)]);
        assert_eq!(db.query_row("SELECT count(*) FROM shell_execution INDEXED BY activity_shell_spawned_turn WHERE turn_id='u' AND CASE WHEN json_valid(shell_json) THEN json_extract(shell_json,'$.spawned')=1 ELSE 1 END", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(
            db.query_row(
                "SELECT shell_json FROM shell_execution WHERE id='bad'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "broken json"
        );
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            crate::storage::SCHEMA_VERSION
        );
        assert_eq!(
            db.query_row(
                "SELECT name FROM schema_migration WHERE version=23",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "0023_activity_bounded"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM thread WHERE id='t'", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        for index in [
            "activity_attempt_turn",
            "activity_turn_run",
            "activity_turn_run_state",
            "activity_group_turn_state",
            "activity_shell_spawned_turn",
            "activity_shell_uncollected_owner",
            "jobs_source",
            "nodes_index_gc",
            "node_event_retention",
            "delivery_event_state",
            "jobs_running_recovery",
            "jobs_running_recovery",
        ] {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    [index],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1,
                "{index}"
            );
        }
    }
}
