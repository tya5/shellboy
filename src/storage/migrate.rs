//! Startup-only schema upgrades. Runtime readers use the current schema only.
use super::{StorageError, migration_integrity::Impact, validate_schema};
use rusqlite::Connection;

mod a_state_scope;
mod summary_attempts;

pub(super) struct Migration {
    pub version: i64,
    pub sql: &'static str,
    pub foreign_keys: Option<Impact>,
}

/// First line of a migration that rebuilds tables: it runs with foreign keys
/// off and is checked before commit. The SQL file is the only list of such
/// migrations, so landing can renumber them by text (#359).
const REBUILD_MARKER: &str = "-- rebuild: foreign keys off";

impl Migration {
    fn rebuilds_tables(&self) -> bool {
        self.sql.lines().next() == Some(REBUILD_MARKER)
    }
}

pub(super) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 2,
        sql: include_str!("../../migrations/0002_gc_indexes.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 3,
        sql: include_str!("../../migrations/0003_context_indexes.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 4,
        sql: include_str!("../../migrations/0004_attachment_provenance.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 5,
        sql: include_str!("../../migrations/0005_origin_credentials.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 6,
        sql: include_str!("../../migrations/0006_receipt_budget.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 7,
        sql: include_str!("../../migrations/0007_usage_and_source_indexes.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 8,
        sql: include_str!("../../migrations/0008_receipt_accounting_compat.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 9,
        sql: include_str!("../../migrations/0009_compaction_turn_indexes.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 10,
        sql: include_str!("../../migrations/0010_hot_query_indexes.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 11,
        sql: include_str!("../../migrations/0011_current_index_metadata.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 12,
        sql: include_str!("../../migrations/0012_revisit_metadata.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 13,
        sql: include_str!("../../migrations/0013_message_path_admission.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 14,
        sql: include_str!("../../migrations/0014_turn_interrupt.sql"),
        foreign_keys: Some(Impact::Tables(&["turn", "draft"])),
    },
    Migration {
        version: 15,
        sql: include_str!("../../migrations/0015_readback_indexes.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 16,
        sql: include_str!("../../migrations/0016_thread_observation.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 17,
        sql: include_str!("../../migrations/0017_thread_parent.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 18,
        sql: include_str!("../../migrations/0018_summary_usage_metadata.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 19,
        sql: include_str!("../../migrations/0019_context_attempt_scope.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 20,
        sql: include_str!("../../migrations/0020_gc_index_source.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 21,
        sql: include_str!("../../migrations/0021_event_retention.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 22,
        sql: include_str!("../../migrations/0022_logical_usage_counters.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 23,
        sql: include_str!("../../migrations/0023_activity_bounded.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 24,
        sql: include_str!("../../migrations/0024_revisit_messages.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 25,
        sql: include_str!("../../migrations/0025_history_attempt_index.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 26,
        sql: include_str!("../../migrations/0026_reply_expected.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 27,
        sql: include_str!("../../migrations/0027_index_recovery.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 28,
        sql: include_str!("../../migrations/0028_tool_group_recovery_candidates.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 29,
        sql: include_str!("../../migrations/0029_active_attempt_recovery_index.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 30,
        sql: include_str!("../../migrations/0030_thread_auto_compact_at.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 31,
        sql: include_str!("../../migrations/0031_thread_summary_parameters.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 32,
        sql: include_str!("../../migrations/0032_thread_archive.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 33,
        sql: include_str!("../../migrations/0033_index_int8_scale.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 34,
        sql: include_str!("../../migrations/0034_thread_tags.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 35,
        sql: include_str!("../../migrations/0035_index_reclaim_lookup.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 36,
        sql: include_str!("../../migrations/0036_spill_tool_metadata.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 37,
        sql: include_str!("../../migrations/0037_thread_branch_kind.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 38,
        sql: include_str!("../../migrations/0038_human_reply_pending.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 39,
        sql: include_str!("../../migrations/0039_independent_runs.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 40,
        sql: include_str!("../../migrations/0040_selection_nonappend_generation.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 41,
        sql: include_str!("../../migrations/0041_receipt_reply_node_lookup.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 42,
        sql: include_str!("../../migrations/0042_notification_semantic_descriptor.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 43,
        sql: include_str!("../../migrations/0043_thread_single_summary_settings.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 44,
        sql: include_str!("../../migrations/0044_thread_max_tokens.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 45,
        sql: include_str!("../../migrations/0045_thread_spill_override.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 46,
        sql: include_str!("../../migrations/0046_scope_identity_v2.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 47,
        sql: include_str!("../../migrations/0047_drop_selection_nonappend_generation.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 48,
        sql: include_str!("../../migrations/0048_human_question_path_admission.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 49,
        sql: include_str!("../../migrations/0049_generated_summary_user_role.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 50,
        sql: include_str!("../../migrations/0050_attachment_page_order.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 51,
        sql: include_str!("../../migrations/0051_thread_schedule.sql"),
        foreign_keys: Some(Impact::Tables(&[
            "schedule",
            "shell_execution",
            "output_chunk",
        ])),
    },
    Migration {
        version: 52,
        sql: include_str!("../../migrations/0052_unified_model_attempt.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 53,
        sql: include_str!("../../migrations/0053_retired_schedule_shells.sql"),
        foreign_keys: Some(Impact::Tables(&["shell_execution", "output_chunk"])),
    },
    Migration {
        version: 54,
        sql: include_str!("../../migrations/0054_thread_created_at.sql"),
        foreign_keys: Some(Impact::Tables(&["thread"])),
    },
    Migration {
        version: 55,
        sql: include_str!("../../migrations/0055_automatic_compaction.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 56,
        sql: include_str!("../../migrations/0056_directional_reply_pending.sql"),
        foreign_keys: None,
    },
    Migration {
        version: 57,
        sql: include_str!("../../migrations/0057_answer_output_limit.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 58,
        sql: include_str!("../../migrations/0058_thread_import_origin.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 59,
        sql: include_str!("../../migrations/0059_thread_artifacts.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 60,
        sql: include_str!("../../migrations/0060_operation_receipt_lane.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 61,
        sql: include_str!("../../migrations/0061_schedule_service_kind.sql"),
        foreign_keys: Some(Impact::Tables(&["schedule", "shell_execution"])),
    },
    Migration {
        version: 62,
        sql: include_str!("../../migrations/0062_instruction_warnings.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 63,
        sql: include_str!("../../migrations/0063_thread_reply_summary.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 64,
        sql: include_str!("../../migrations/0064_thread_cwd_partitions.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 65,
        sql: include_str!("../../migrations/0065_run_request_ceiling.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 66,
        sql: include_str!("../../migrations/0066_run_stop_settlement_failure.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 67,
        sql: include_str!("../../migrations/0067_open_reply_duties.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 68,
        sql: include_str!("../../migrations/0068_schedule_notify.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 69,
        sql: include_str!("../../migrations/0069_instruction_snapshot_turns.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 70,
        sql: include_str!("../../migrations/0070_artifact_current_scheme.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 71,
        sql: include_str!("../../migrations/0071_service_name.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 72,
        sql: include_str!("../../migrations/0072_attachment_omission.sql"),
        // The new column and backfill do not change any foreign-key edges.
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 73,
        sql: include_str!("../../migrations/next_accepted_receipt_turn.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 74,
        sql: include_str!("../../migrations/next_latest_adopted_attempt.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
    Migration {
        version: 75,
        sql: include_str!("../../migrations/next_replay_path_revision.sql"),
        foreign_keys: Some(Impact::IndexOnly),
    },
];

pub(super) fn run(
    connection: &mut Connection,
    version: i64,
    report: &dyn Fn(i64, i64),
    validation: &dyn Fn(),
) -> Result<(), StorageError> {
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version > version)
    {
        report(migration.version - version, super::SCHEMA_VERSION - version);
        let apply = |connection: &mut Connection| -> Result<(), StorageError> {
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if migration.version == 51 {
                use rusqlite::OptionalExtension;
                let malformed: Option<String> = transaction.query_row(
                    "SELECT id FROM shell_execution WHERE NOT json_valid(shell_json) ORDER BY id LIMIT 1",
                    [], |row| row.get(0),
                ).optional()?;
                if let Some(id) = malformed {
                    return Err(StorageError::Schema(format!(
                        "migration 51 cannot rebuild shell execution {id:?}: invalid shell_json"
                    )));
                }
            }

            let additive_column_already_present = match migration.version {
            36 => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('node') WHERE name='spill_tools_json')",
                [],
                |r| r.get::<_, bool>(0),
            )?,
            37 => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('thread') WHERE name='branch_kind')",
                [],
                |r| r.get::<_, bool>(0),
            )?,
            40 => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('thread') WHERE name='selection_nonappend_generation')",
                [],
                |r| r.get::<_, bool>(0),
            )?,
            _ => false,
        };
            if additive_column_already_present {
                // Downgrade fixtures can retain additive columns; keep existing data intact.
                transaction.execute_batch(
                    migration
                        .sql
                        .split_once(';')
                        .expect("column then metadata")
                        .1,
                )?;
            } else {
                transaction.execute_batch(migration.sql)?;
            }
            if migration.version == 46 {
                a_state_scope::convert_rows(&transaction)?;
            }
            if migration.version == 49 {
                migrate_generated_summary_user_roles(&transaction)?;
            }
            if migration.version == 4 {
                migrate_attachment_sources(&transaction)?;
            }
            if migration.version == 52 {
                summary_attempts::convert(&transaction)?;
                transaction.execute_batch(include_str!("migrate/retire_summary_ledger.sql"))?;
            }
            if migration.rebuilds_tables() {
                super::migration_integrity::Check::for_migration(Some(migration))
                    .run(&transaction)?;
            }
            transaction.commit()?;
            Ok(())
        };
        if migration.rebuilds_tables() {
            with_foreign_keys_disabled(connection, apply)?;
        } else {
            apply(connection)?;
        }
    }
    validation();
    validate_schema(connection)?;
    Ok(())
}

// The transaction belongs to the callback, so it unwinds/rolls back before this
// outer guard restores enforcement. Never toggle foreign_keys inside BEGIN.
struct ForeignKeysDisabled<'a> {
    connection: &'a mut Connection,
    restored: bool,
}
impl Drop for ForeignKeysDisabled<'_> {
    fn drop(&mut self) {
        if !self.restored {
            // Explicit restoration reports errors normally; Drop also covers panic.
            let _ = self.connection.pragma_update(None, "foreign_keys", true);
        }
    }
}
fn with_foreign_keys_disabled<T>(
    connection: &mut Connection,
    operation: impl FnOnce(&mut Connection) -> Result<T, StorageError>,
) -> Result<T, StorageError> {
    if !connection.is_autocommit() {
        return Err(StorageError::Schema(
            "FK rebuild requires an outer transaction boundary".into(),
        ));
    }
    let mut guard = ForeignKeysDisabled {
        connection,
        restored: false,
    };
    guard
        .connection
        .pragma_update(None, "foreign_keys", false)?;
    let result = operation(guard.connection);
    guard.connection.pragma_update(None, "foreign_keys", true)?;
    guard.restored = true;
    result
}

/// Convert only canonical, completed generated summaries. Replacement source
/// snapshots are immutable; the history node payload is the mutable presentation.
fn migrate_generated_summary_user_roles(
    tx: &rusqlite::Transaction<'_>,
) -> Result<(), StorageError> {
    use rusqlite::params;
    use serde_json::Value;

    const FORMAT: &str = "shellboy-summary-v1";
    let nodes = tx
        .prepare(
            "SELECT id,payload_json,payload_bytes,body_hash,payload_revision \
             FROM node WHERE kind='summary' AND payload_json IS NOT NULL \
             AND deleted_reason IS NULL ORDER BY id",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    for (node_id, raw, payload_bytes, body_hash, revision) in nodes {
        let Ok(mut payload) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if payload.get("summary_format").and_then(Value::as_str) != Some(FORMAT) {
            continue;
        }
        let Some(item) = payload.get("item").and_then(Value::as_object) else {
            continue;
        };
        if item.get("role").and_then(Value::as_str) != Some("developer") {
            continue;
        }
        let item_keys_valid = item
            .keys()
            .all(|key| matches!(key.as_str(), "role" | "content" | "reasoning_epoch"));
        let content = item.get("content").and_then(Value::as_array);
        let content_valid = content.is_some_and(|parts| {
            parts.len() == 1
                && parts[0].as_object().is_some_and(|part| {
                    part.len() == 2
                        && part.get("type").and_then(Value::as_str) == Some("input_text")
                        && part.get("text").and_then(Value::as_str).is_some()
                })
        });
        if !item_keys_valid
            || !content_valid
            || item
                .get("reasoning_epoch")
                .is_some_and(|epoch| !epoch.is_i64())
        {
            continue;
        }
        let Some(source_ids) = payload.get("source_node_ids").and_then(Value::as_array) else {
            continue;
        };
        let Some(source_ids) = source_ids
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        // The output-node ID is not unique. Any duplicate/contradictory replacement
        // makes the legacy role ambiguous and must remain unchanged.
        let replacements = tx
            .prepare(
                "SELECT id,format_version,state FROM replacement \
                 WHERE output_node_id=?1 ORDER BY id",
            )?
            .query_map([&node_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let [(replacement_id, format, state)] = replacements.as_slice() else {
            continue;
        };
        if format.as_str() != FORMAT || state.as_str() != "completed" {
            continue;
        }
        let recorded_sources = tx
            .prepare(
                "SELECT ordinal,source_node_id FROM replacement_source \
                 WHERE replacement_id=?1 ORDER BY ordinal",
            )?
            .query_map([replacement_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if recorded_sources.len() != source_ids.len()
            || recorded_sources.iter().enumerate().any(
                |(ordinal, (recorded_ordinal, source_id))| {
                    *recorded_ordinal != ordinal as i64 || source_id.as_str() != source_ids[ordinal]
                },
            )
        {
            continue;
        }
        let encoded = serde_json::to_vec(&payload)
            .map_err(|error| StorageError::Schema(error.to_string()))?;
        if encoded.as_slice() != raw.as_bytes() {
            continue;
        }
        let expected_hash = crate::digest::sha256_hex(&encoded);
        if payload_bytes != raw.len() as i64 || body_hash.as_deref() != Some(expected_hash.as_str())
        {
            continue;
        }

        payload["item"]["role"] = Value::String("user".into());
        let updated = serde_json::to_string(&payload)
            .map_err(|error| StorageError::Schema(error.to_string()))?;
        let bytes = updated.len() as i64;
        let hash = crate::digest::sha256_hex(updated.as_bytes());
        let changed = tx.execute(
            "UPDATE node SET payload_json=?2,payload_bytes=?3,body_hash=?4, \
             payload_revision=payload_revision+1 WHERE id=?1 AND payload_revision=?5 AND payload_json=?6",
            params![node_id, updated, bytes, hash, revision, raw],
        )?;
        if changed != 1 {
            return Err(StorageError::Schema(
                "generated summary changed during startup migration".into(),
            ));
        }
    }
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
mod question_admission_migration_tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;

    fn schema47() -> Connection {
        let db = crate::storage::historical_memory(47);
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now')", []).unwrap();
        db
    }

    fn receipt(db: &Connection, id: &str, state: &str, result: serde_json::Value) {
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'assistant','now','{}')",
            [id],
        )
        .unwrap();
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,path_admission,created_at,result_json)VALUES(?1,?1,'null','t',?1,?2,'unadmitted','now',?3)",params![id,state,result.to_string()]).unwrap();
    }

    fn migrate(db: &mut Connection) -> Result<(), StorageError> {
        run(db, 47, &|_, _| {}, &|| {})
    }

    #[test]
    fn published_applied_human_questions_settle_path_admission_across_all_reply_states() {
        let mut db = schema47();
        for (id, reply) in [
            ("pending", Some("pending")),
            ("replied", Some("replied")),
            ("cancelled", Some("cancelled")),
            ("recipient-deleted", Some("recipient_deleted")),
            ("missing", None),
        ] {
            let mut value = json!({"to_human":true});
            if let Some(reply) = reply {
                value["reply"] = json!({"state":reply});
            }
            receipt(&db, id, "applied", value);
        }
        receipt(
            &db,
            "accepted",
            "accepted",
            json!({"to_human":true,"reply":{"state":"pending"}}),
        );
        receipt(
            &db,
            "held",
            "held",
            json!({"to_human":true,"reply":{"state":"pending"}}),
        );
        receipt(
            &db,
            "cancelled-receipt",
            "cancelled",
            json!({"to_human":true,"reply":{"state":"pending"}}),
        );
        receipt(
            &db,
            "string-not-bool",
            "applied",
            json!({"to_human":"true","reply":{"state":"pending"}}),
        );
        receipt(
            &db,
            "nonhuman",
            "applied",
            json!({"to_human":false,"reply":{"state":"pending"}}),
        );
        migrate(&mut db).unwrap();
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            super::super::SCHEMA_VERSION
        );
        assert_eq!(db.query_row("SELECT count(*) FROM schema_migration WHERE version=49 AND name='0049_generated_summary_user_role'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(db.query_row("SELECT count(*) FROM schema_migration WHERE version=50 AND name='0050_attachment_page_order'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        for id in [
            "pending",
            "replied",
            "cancelled",
            "recipient-deleted",
            "missing",
        ] {
            let admission: String = db
                .query_row(
                    "SELECT path_admission FROM receipt WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            let reply = crate::control::db::named_reply_state_of_receipt(&db, id);
            assert_eq!(admission, "admitted", "{id}");
            let expected = match id {
                "pending" => Some("pending"),
                "replied" => Some("replied"),
                "cancelled" => Some("cancelled"),
                "recipient-deleted" => Some("recipient_deleted"),
                _ => None,
            };
            assert_eq!(
                reply.as_deref(),
                expected,
                "reply fact is preserved for {id}"
            );
        }
        for id in [
            "accepted",
            "held",
            "cancelled-receipt",
            "string-not-bool",
            "nonhuman",
        ] {
            assert_eq!(
                db.query_row(
                    "SELECT path_admission FROM receipt WHERE id=?1",
                    [id],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "unadmitted",
                "{id}"
            );
        }
        let admitted_before_rerun: Vec<(String, String, Option<String>)> = db
            .prepare("SELECT id,path_admission FROM receipt ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .into_iter()
            .map(|(id, admission)| {
                let reply = crate::control::db::named_reply_state_of_receipt(&db, &id);
                (id, admission, reply)
            })
            .collect();
        run(&mut db, super::super::SCHEMA_VERSION, &|_, _| {}, &|| {}).unwrap();
        let admitted_after_rerun: Vec<(String, String, Option<String>)> = db
            .prepare("SELECT id,path_admission FROM receipt ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .into_iter()
            .map(|(id, admission)| {
                let reply = crate::control::db::named_reply_state_of_receipt(&db, &id);
                (id, admission, reply)
            })
            .collect();
        assert_eq!(admitted_after_rerun, admitted_before_rerun);
    }

    #[test]
    fn v48_migration_and_real_rewind_do_not_reappend_legacy_question_ancestor() {
        let mut db = schema47();
        {
            let tx = db.transaction().unwrap();
            tx.execute_batch(
                "INSERT INTO run(id,thread_id,state,created_at) VALUES('legacy-run','t','completed','2020-01-01');\
                 INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('legacy-turn','t','legacy-run','completed','2020-01-01');",
            ).unwrap();
            // Published MessageAsk rows use assistant nodes with no Run/Turn owner.
            for (id, kind, turn_id, at) in [
                (
                    "root",
                    "message",
                    Some("legacy-turn"),
                    "2020-01-01T00:00:00Z",
                ),
                (
                    "cursor",
                    "message",
                    Some("legacy-turn"),
                    "2020-01-01T00:00:02Z",
                ),
                (
                    "tail",
                    "message",
                    Some("legacy-turn"),
                    "2020-01-01T00:00:03Z",
                ),
                ("q-preserve", "assistant", None, "2020-01-01T00:00:01Z"),
                ("q-off-path", "assistant", None, "2020-01-01T00:00:00Z"),
                (
                    "q-off-path-successor",
                    "message",
                    Some("legacy-turn"),
                    "2020-01-01T00:00:01Z",
                ),
            ] {
                let payload = json!({"text":id});
                tx.execute(
                    "INSERT INTO node(id,turn_id,kind,created_at,payload_json) VALUES(?1,?2,?3,?4,?5)",
                    params![id,turn_id,kind,at,payload.to_string()],
                ).unwrap();
            }
            tx.execute_batch(
                "INSERT INTO edge(src,dst) VALUES('root','cursor'),('cursor','q-preserve'),('q-preserve','tail'),('q-off-path','q-off-path-successor'),('q-off-path-successor','cursor');\
                 INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('t',0,'root'),('t',1,'cursor'),('t',2,'q-preserve'),('t',3,'tail');\
                 UPDATE thread SET cursor_node_id='tail' WHERE id='t';",
            ).unwrap();
            for (receipt, node) in [
                ("legacy-preserve", "q-preserve"),
                ("legacy-ancestor", "q-off-path"),
            ] {
                let result = json!({"to_human":true,"reply":{"state":"pending"}}).to_string();
                tx.execute(
                    "INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,path_admission,created_at,result_json) VALUES(?1,?1,'{\"kind\":\"model\",\"thread_id\":\"sender\"}','t',?2,'applied','unadmitted','2020-01-01T00:00:01Z',?3)",
                    params![receipt,node,result],
                ).unwrap();
            }
            tx.commit().unwrap();
        }

        // Both rows satisfy the old writer's positive migration inputs and sort
        // before the later current-work frontier. The off-path Q really reaches
        // the selected cursor through graph ancestry, despite not being selected.
        let legacy_candidates: Vec<String> = db.prepare(
            "SELECT r.message_node_id FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.message_node_id IN ('q-preserve','q-off-path') AND r.target_thread_id='t' AND r.state='applied' AND r.path_admission='unadmitted' AND r.run_id IS NULL AND r.turn_id IS NULL AND n.kind='assistant' AND n.turn_id IS NULL AND r.created_at<'2021-01-01T00:00:01Z' ORDER BY r.created_at,r.rowid",
        ).unwrap().query_map([],|row|row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        for id in &legacy_candidates {
            let details = crate::control::db::message_details(&db, id).unwrap();
            assert!(details.to_human);
            assert!(matches!(
                details.reply.as_ref().and_then(|r| r["state"].as_str()),
                Some("pending" | "replied")
            ));
        }
        assert_eq!(legacy_candidates, ["q-preserve", "q-off-path"]);
        assert!(
            db.query_row(
                "WITH RECURSIVE reachable(id) AS (SELECT dst FROM edge WHERE src='q-off-path' UNION SELECT e.dst FROM edge e JOIN reachable r ON e.src=r.id) SELECT EXISTS(SELECT 1 FROM reachable WHERE id='cursor')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap(),
            "cursor must be reachable from the off-selected-path Q before the rewind"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM thread_path WHERE thread_id='t' AND node_id='q-off-path'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );

        migrate(&mut db).unwrap();
        for id in ["legacy-preserve", "legacy-ancestor"] {
            let (receipt_state, admission): (String, String) = db
                .query_row(
                    "SELECT state,path_admission FROM receipt WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let state = (
                receipt_state,
                admission,
                crate::control::db::named_reply_state_of_receipt(&db, id),
            );
            assert_eq!(
                state,
                ("applied".into(), "admitted".into(), Some("pending".into()))
            );
        }

        let branch = "rewind-branch";
        {
            let tx = db.transaction().unwrap();
            crate::history::rewind::rewind_thread(
                &tx,
                "t",
                "cursor",
                false,
                false,
                branch,
                "2021-01-01T00:00:00Z",
            )
            .unwrap();
            tx.commit().unwrap();
        }
        assert!(
            db.query_row(
                "WITH RECURSIVE reachable(id) AS (SELECT dst FROM edge WHERE src='q-off-path' UNION SELECT e.dst FROM edge e JOIN reachable r ON e.src=r.id) SELECT EXISTS(SELECT 1 FROM reachable WHERE id='cursor')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap(),
            "real rewind preserves graph ancestry from the off-path Q to the retained cursor"
        );
        assert!(!db.query_row(
            "WITH RECURSIVE reachable(id) AS (SELECT dst FROM edge WHERE src='q-preserve' UNION SELECT e.dst FROM edge e JOIN reachable r ON e.src=r.id) SELECT EXISTS(SELECT 1 FROM reachable WHERE id='cursor')",
            [],
            |row| row.get::<_, bool>(0),
        ).unwrap(), "removed preservation Q does not point back to the retained cursor");
        let parent_path: Vec<String> = db
            .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let branch_path: Vec<String> = db
            .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")
            .unwrap()
            .query_map([branch], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(parent_path, ["root", "cursor"]);
        assert_eq!(branch_path, ["root", "cursor", "q-preserve", "tail"]);
        assert!(!parent_path.contains(&"q-preserve".to_owned()));
        assert!(branch_path.contains(&"q-preserve".to_owned()));

        db.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('current-input','user','2021-01-01T00:00:01Z','{}')",[]).unwrap();
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,path_admission,created_at,result_json) VALUES('current-input','current-input','{\"kind\":\"human\"}','t','current-input','accepted','unadmitted','2021-01-01T00:00:01Z','{}')",[]).unwrap();
        let tx = db.transaction().unwrap();
        let revision =
            crate::history::selection_revision(&tx, "t", crate::history::ThreadScope::Live)
                .unwrap();
        let current = vec!["current-input".to_owned()];
        let plan = crate::control::message_delivery::plan_admission_tx(
            &tx,
            "t",
            &current,
            "2021-01-01T00:00:01Z",
        )
        .unwrap();
        assert_eq!(plan.admitted_nodes, current);
        assert_eq!(plan.admitted_callers, ["current-input"]);
        crate::control::message_delivery::append_admission_plan_tx(
            &tx,
            "t",
            revision as u64,
            &plan,
            "2021-01-01T00:00:01Z",
        )
        .unwrap();
        tx.commit().unwrap();

        let final_path: Vec<String> = db
            .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(final_path, ["root", "cursor", "current-input"]);
        let branch_after_append: Vec<String> = db
            .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")
            .unwrap()
            .query_map([branch], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            branch_after_append,
            ["root", "cursor", "q-preserve", "tail"]
        );
        assert!(branch_after_append.contains(&"q-preserve".to_owned()));
        assert!(!final_path.contains(&"q-preserve".to_owned()));
        assert!(!final_path.contains(&"q-off-path".to_owned()));
        let current_state:(String,String,bool)=db.query_row(
            "SELECT r.state,r.path_admission,EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id='t' AND p.node_id=r.message_node_id) FROM receipt r WHERE r.id='current-input'",
            [],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).unwrap();
        assert_eq!(current_state, ("accepted".into(), "admitted".into(), true));
        for (receipt, node) in [
            ("legacy-preserve", "q-preserve"),
            ("legacy-ancestor", "q-off-path"),
        ] {
            let owner: (String, String, String, Option<String>, Option<String>, String, String) = db
                .query_row(
                    "SELECT id,client_operation_id,target_thread_id,run_id,turn_id,state,path_admission FROM receipt WHERE id=?1 AND message_node_id=?2",
                    params![receipt,node],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
                )
                .unwrap();
            assert_eq!(
                owner,
                (
                    receipt.into(),
                    receipt.into(),
                    "t".into(),
                    None,
                    None,
                    "applied".into(),
                    "admitted".into()
                )
            );
        }
        for id in ["legacy-preserve", "legacy-ancestor"] {
            let (receipt_state, admission): (String, String) = db
                .query_row(
                    "SELECT state,path_admission FROM receipt WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let state = (
                receipt_state,
                admission,
                crate::control::db::named_reply_state_of_receipt(&db, id),
            );
            assert_eq!(
                state,
                ("applied".into(), "admitted".into(), Some("pending".into()))
            );
        }
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            super::super::SCHEMA_VERSION
        );
        assert_eq!(db.query_row("SELECT count(*) FROM schema_migration WHERE version=49 AND name='0049_generated_summary_user_role'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(db.query_row("SELECT count(*) FROM schema_migration WHERE version=50 AND name='0050_attachment_page_order'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        let path_before_rerun: Vec<String> = db
            .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let receipts_before_rerun: Vec<(String, String)> = db
            .prepare("SELECT id,path_admission FROM receipt WHERE id IN ('legacy-preserve','legacy-ancestor') ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?,r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        run(&mut db, super::super::SCHEMA_VERSION, &|_, _| {}, &|| {}).unwrap();
        let path_after_rerun: Vec<String> = db
            .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let receipts_after_rerun: Vec<(String, String)> = db
            .prepare("SELECT id,path_admission FROM receipt WHERE id IN ('legacy-preserve','legacy-ancestor') ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?,r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(path_after_rerun, path_before_rerun);
        assert_eq!(receipts_after_rerun, receipts_before_rerun);
    }

    #[test]
    fn failed_schema48_step_rolls_back_path_update_ledger_and_version() {
        let mut db = schema47();
        receipt(
            &db,
            "historical",
            "applied",
            json!({"to_human":true,"reply":{"state":"pending"}}),
        );
        db.execute_batch("CREATE TRIGGER reject_schema48_ledger BEFORE INSERT ON schema_migration WHEN NEW.version=48 AND EXISTS(SELECT 1 FROM receipt WHERE id='historical' AND path_admission='admitted') BEGIN SELECT RAISE(ABORT,'synthetic refusal after path backfill'); END;").unwrap();
        let error = migrate(&mut db).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("synthetic refusal after path backfill")
        );
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            47
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM schema_migration WHERE version=48",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM schema_migration WHERE version=49",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            (
                db.query_row(
                    "SELECT path_admission FROM receipt WHERE id='historical'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                crate::control::db::named_reply_state_of_receipt(&db, "historical").unwrap()
            ),
            ("unadmitted".into(), "pending".into())
        );
    }
}

#[cfg(test)]
mod activity_migration_tests {
    use super::*;

    #[test]
    fn historical_table_rebuilds_carry_the_marker() {
        let rebuilds: Vec<_> = MIGRATIONS
            .iter()
            .filter(|migration| migration.rebuilds_tables())
            .map(|migration| migration.version)
            .collect();
        for version in [39, 51, 52, 53] {
            assert!(rebuilds.contains(&version), "{version}");
        }
    }

    #[test]
    fn executor_matches_registered_migration_versions() {
        // Schema1 is bootstrap-only; every later registered migration must
        // have exactly one executable step, in registry order.
        let registered: Vec<_> = crate::storage::MIGRATION_NAMES
            .iter()
            .skip(1)
            .map(|(version, _)| *version)
            .collect();
        let executable: Vec<_> = MIGRATIONS
            .iter()
            .map(|migration| migration.version)
            .collect();
        assert_eq!(executable, registered);
    }

    #[tokio::test]
    async fn pinned_schema42_startup_upgrade_writer_settings_and_reopen() {
        // Pinned verbatim from 4e6241a9:src/storage/current.sql, not derived
        // by downgrading today's schema or replaying the executor under test.
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("conversation.sqlite");
        let usage = r#"{"id":"saved","thread_id":"old","profile_id":"p","model_id":"m","started_at":"2026-10-31T00:00:00Z","updated_at":"2026-10-31T00:00:00Z","requests":3,"map_requests":2,"reduce_requests":1,"oversized_reduce_parts":4,"input_tokens":{"known_sum":10,"unknown_count":0,"overflow":false},"cached_tokens":{"known_sum":10,"unknown_count":0,"overflow":false},"cache_creation_tokens":{"known_sum":10,"unknown_count":0,"overflow":false},"output_tokens":{"known_sum":10,"unknown_count":0,"overflow":false}}"#;
        let node = r#"{"summary_usage":{"map_requests":4,"reduce_requests":5,"oversized_reduce_parts":6},"kept":"safe"}"#;
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(include_str!("../../tests/fixtures/compaction-schema42.sql"))
                .unwrap();
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                42
            );
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at,segment_chars,reduce_rounds) VALUES('old','/','','p','m','now',48000,3)",[]).unwrap();
            db.execute(
                "INSERT INTO app_meta(key,value) VALUES('summary.usage.old.saved',?1)",
                [usage],
            )
            .unwrap();
            db.execute(
                "INSERT INTO node(id,kind,created_at,payload_json) VALUES('summary-old','summary','now',?1)",
                [node],
            )
            .unwrap();
        }
        // open_connection is the actual startup migration path. Keep this
        // connection open while the foreground DbWorker owns a second handle.
        let _startup = crate::storage::open_connection(&path, None, None, false).unwrap();
        let writer = crate::storage::DbWorker::open(&path).await.unwrap();
        writer.call(move |db| {
            assert_eq!(db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?,crate::storage::SCHEMA_VERSION);
            let old:(Option<i64>,Option<i64>)=db.query_row("SELECT auto_compact_target_percent,summary_max_chars FROM thread WHERE id='old'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert_eq!(old,(None,None));
            let columns:Vec<String>=db.prepare("PRAGMA table_info(thread)")?.query_map([],|r|r.get(1))?.collect::<rusqlite::Result<_>>()?;
            assert!(!columns.iter().any(|name|matches!(name.as_str(),"segment_chars"|"reduce_rounds")));
            // No new table, and saved history/usage is not rewritten.
            let tables:i64=db.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('thread_retired_summary_settings','summary_usage_stage_archive')",[],|r|r.get(0))?;
            assert_eq!(tables,0);
            assert_eq!(db.query_row("SELECT count(*) FROM model_attempt WHERE kind='summary' AND thread_id='old'",[],|r|r.get::<_,i64>(0))?,3);
            let summary:String=db.query_row("SELECT payload_json FROM node WHERE id='summary-old'",[],|r|r.get(0))?;
            assert_eq!(summary,node);
            assert!(db.prepare("PRAGMA foreign_key_check").unwrap().query([])?.next()?.is_none());
            db.execute("UPDATE thread SET auto_compact_target_percent=80,summary_max_chars=24000 WHERE id='old'",[])?;
            Ok(())
        }).await.unwrap();
        writer.shutdown().await.unwrap();
        let reopened = crate::storage::DbWorker::open(&path).await.unwrap();
        reopened.call(|db| {
            let saved:(i64,i64)=db.query_row("SELECT auto_compact_target_percent,summary_max_chars FROM thread WHERE id='old'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert_eq!(saved,(80,24000));
            assert_eq!(db.query_row("SELECT count(*) FROM schema_migration WHERE version=43",[],|r|r.get::<_,i64>(0))?,1);
            Ok(())
        }).await.unwrap();
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pinned_schema43_startup_upgrade_adds_unset_thread_max_tokens() {
        // Pinned verbatim from 441750809e:src/storage/current.sql (v0.12.0),
        // not derived by downgrading today's schema.
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("conversation.sqlite");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(include_str!(
                "../../tests/fixtures/storage-current-schema43.sql"
            ))
            .unwrap();
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                43
            );
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at,auto_compact_target_percent,summary_max_chars) VALUES('old','/','','p','m','now',80,24000)",[]).unwrap();
        }
        let _startup = crate::storage::open_connection(&path, None, None, false).unwrap();
        let writer = crate::storage::DbWorker::open(&path).await.unwrap();
        writer.call(move |db| {
            assert_eq!(db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?,crate::storage::SCHEMA_VERSION);
            assert_eq!(db.query_row("SELECT name FROM schema_migration WHERE version=44",[],|r|r.get::<_,String>(0))?,"0044_thread_max_tokens");
            assert_eq!(db.query_row("SELECT name FROM schema_migration WHERE version=45",[],|r|r.get::<_,String>(0))?,"0045_thread_spill_override");
            // Existing Threads keep the connection value: both new columns are unset.
            let old:(Option<i64>,i64,i64,Option<i64>)=db.query_row("SELECT max_tokens,auto_compact_target_percent,summary_max_chars,spill_override FROM thread WHERE id='old'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            assert_eq!(old,(None,80,24000,None));
            for bad in ["0","-1","1.5","'x'"] {
                assert!(db.execute(&format!("UPDATE thread SET max_tokens={bad} WHERE id='old'"),[]).is_err(),"{bad}");
            }
            db.execute("UPDATE thread SET max_tokens=9223372036854775807 WHERE id='old'",[])?;
            db.execute("UPDATE thread SET max_tokens=64000 WHERE id='old'",[])?;
            assert!(db.prepare("PRAGMA foreign_key_check").unwrap().query([])?.next()?.is_none());
            Ok(())
        }).await.unwrap();
        writer.shutdown().await.unwrap();
        let reopened = crate::storage::DbWorker::open(&path).await.unwrap();
        reopened
            .call(|db| {
                assert_eq!(
                    db.query_row("SELECT max_tokens FROM thread WHERE id='old'", [], |r| r
                        .get::<_, i64>(0))?,
                    64000
                );
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM schema_migration WHERE version=44",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    1
                );
                Ok(())
            })
            .await
            .unwrap();
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn synthetic_schema44_upgrade_adds_checked_nullable_spill_override() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("synthetic44.sqlite");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(include_str!(
                "../../tests/fixtures/storage-current-schema43.sql"
            ))
            .unwrap();
            db.execute_batch(include_str!("../../migrations/0044_thread_max_tokens.sql"))
                .unwrap();
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('old','/','','p','m','now'),('unrelated','/other','','p','m','now')",[]).unwrap();
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                44
            );
        }
        let worker = crate::storage::DbWorker::open(&path).await.unwrap();
        worker
            .call(|db| {
                assert_eq!(
                    db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
                    crate::storage::SCHEMA_VERSION
                );
                assert_eq!(
                    db.query_row(
                        "SELECT spill_override FROM thread WHERE id='old'",
                        [],
                        |r| r.get::<_, Option<i64>>(0)
                    )?,
                    None
                );
                assert_eq!(
                    db.query_row(
                        "SELECT name FROM schema_migration WHERE version=45",
                        [],
                        |r| r.get::<_, String>(0)
                    )?,
                    "0045_thread_spill_override"
                );
                assert_eq!(
                    db.query_row(
                        "SELECT spill_override FROM thread WHERE id='unrelated'",
                        [],
                        |r| r.get::<_, Option<i64>>(0)
                    )?,
                    None
                );
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM thread WHERE id IN ('old','unrelated')",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    2
                );
                db.execute("UPDATE thread SET spill_override=0 WHERE id='old'", [])?;
                assert_eq!(
                    db.query_row(
                        "SELECT spill_override FROM thread WHERE id='old'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                db.execute("UPDATE thread SET spill_override=1 WHERE id='old'", [])?;
                for bad in ["2", "'bad'", "0.5"] {
                    assert!(
                        db.execute(
                            &format!("UPDATE thread SET spill_override={bad} WHERE id='old'"),
                            []
                        )
                        .is_err(),
                        "{bad}"
                    );
                }
                assert!(
                    db.prepare("PRAGMA foreign_key_check")?
                        .query([])?
                        .next()?
                        .is_none()
                );
                Ok(())
            })
            .await
            .unwrap();
        worker.shutdown().await.unwrap();
    }

    #[test]
    fn rebuild_fk_violation_rejects_commit_preserves_old_rows_and_restores_enforcement() {
        let mut db = crate::storage::historical_memory(38);
        db.pragma_update(None, "foreign_keys", false).unwrap();
        db.execute_batch("CREATE TABLE synthetic_reference(run_id TEXT REFERENCES run(id)); INSERT INTO synthetic_reference VALUES('missing');").unwrap();
        db.pragma_update(None, "foreign_keys", true).unwrap();
        let before: Vec<String> = db
            .prepare(
                "SELECT sql FROM sqlite_schema WHERE name IN ('run','runs_by_owner') ORDER BY name",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let error = run(&mut db, 38, &|_, _| {}, &|| {}).unwrap_err();
        assert!(
            matches!(error,StorageError::Schema(ref message) if message=="foreign key violations")
        );
        assert!(db.is_autocommit());
        assert_eq!(
            db.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            38
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM schema_migration WHERE version=39",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT run_id FROM synthetic_reference", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "missing"
        );
        let after: Vec<String> = db
            .prepare(
                "SELECT sql FROM sqlite_schema WHERE name IN ('run','runs_by_owner') ORDER BY name",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn rebuild_panic_rolls_back_before_restoring_foreign_keys() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE witness(value TEXT); INSERT INTO witness VALUES('before');").unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), StorageError> = with_foreign_keys_disabled(&mut db, |db| {
                let tx = db.transaction()?;
                tx.execute("UPDATE witness SET value='uncommitted'", [])?;
                panic!("synthetic FK rebuild panic");
            });
        }));
        assert!(panic.is_err());
        assert!(db.is_autocommit());
        assert_eq!(
            db.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT value FROM witness", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "before"
        );
    }

    #[test]
    fn foreign_key_check_detects_violations_with_enforcement_disabled() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE parent(id TEXT PRIMARY KEY); CREATE TABLE child(parent_id TEXT REFERENCES parent(id));").unwrap();
        db.pragma_update(None, "foreign_keys", false).unwrap();
        let tx = db.transaction().unwrap();
        tx.execute("INSERT INTO child VALUES('missing')", [])
            .unwrap();
        assert_eq!(
            tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            1
        );
        tx.rollback().unwrap();
        db.pragma_update(None, "foreign_keys", true).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
    }

    #[test]
    fn failed_run_rebuild_rolls_back_and_restores_foreign_keys() {
        let mut db = crate::storage::historical_memory(38);
        db.pragma_update(None, "foreign_keys", true).unwrap();
        // Inject a collision after FK checks are disabled, before any Run copy.
        db.execute_batch("CREATE TABLE independent_run(collision INTEGER);")
            .unwrap();
        assert!(run(&mut db, 38, &|_, _| {}, &|| {}).is_err());
        assert_eq!(
            db.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            38
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='run'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn recovery_index_upgrades_from_each_recent_schema() {
        for start in [24, 25, 26, 27, 28] {
            let mut db = crate::storage::historical_memory(start as usize);
            run(&mut db, start, &|_, _| {}, &|| {}).unwrap();
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                super::super::SCHEMA_VERSION
            );
            assert_eq!(
                db.query_row(
                    "SELECT name FROM schema_migration WHERE version=28",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "0028_tool_group_recovery_candidates"
            );
            if start < 27 {
                let plan: String = db.query_row("EXPLAIN QUERY PLAN SELECT id FROM embedding_job WHERE state='running' AND id>?1 ORDER BY id LIMIT 128", [""], |r| r.get(3)).unwrap();
                assert!(plan.contains("jobs_running_recovery"), "{plan}");
            }
            let plan: String = db.query_row("EXPLAIN QUERY PLAN SELECT group_id FROM tool_group_recovery_candidate WHERE group_id>?1 ORDER BY group_id LIMIT 32", [""], |r| r.get(3)).unwrap();
            assert!(plan.contains("tool_group_recovery_candidate"), "{plan}");
            let attempt_plan: String = db.query_row("EXPLAIN QUERY PLAN UPDATE model_attempt SET state='incomplete' WHERE state IN ('preparing','inflight')", [], |r| r.get(3)).unwrap();
            assert!(
                attempt_plan.contains("model_attempt_active_recovery"),
                "{attempt_plan}"
            );
        }
    }

    #[test]
    fn active_attempt_recovery_index_updates_only_interruptible_attempts() {
        let mut db = crate::storage::historical_memory(28);
        db.execute_batch("
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/a','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','stopped','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','stopped','now');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES
              ('preparing','u',0,0,'p','m',0,'preparing','{}','now'),
              ('inflight','u',1,0,'p','m',0,'inflight','{}','now'),
              ('completed','u',2,0,'p','m',0,'completed','{}','now');").unwrap();
        run(&mut db, 28, &|_, _| {}, &|| {}).unwrap();
        let changed = db.execute("UPDATE model_attempt SET state='incomplete',ended_at='now',error_json='{}' WHERE state IN ('preparing','inflight')", []).unwrap();
        assert_eq!(changed, 2);
        let states: Vec<(String, String)> = db
            .prepare("SELECT id,state FROM model_attempt ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            states,
            vec![
                ("completed".into(), "completed".into()),
                ("inflight".into(), "incomplete".into()),
                ("preparing".into(), "incomplete".into())
            ]
        );
    }

    #[test]
    fn recovery_candidate_migration_backfills_preexisting_rows() {
        let mut db = crate::storage::historical_memory(27);
        db.execute_batch("
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','old');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','stopped','old');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','stopped','old');
            INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('n','u','tool_result','old','{}');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','old');
            INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('old-open','a','u','running','old'),('old-done','a','u','completed','old'),('old-missing','a','u','completed','old');
            INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('old-done','done',0,'shell','{}','completed'),('old-missing','missing',0,'shell','{}','completed');
            INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('old-done','done','n','completed');").unwrap();
        run(&mut db, 27, &|_, _| {}, &|| {}).unwrap();
        let candidates: Vec<String> = db
            .prepare("SELECT group_id FROM tool_group_recovery_candidate ORDER BY group_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(candidates, vec!["old-missing", "old-open"]);
    }

    #[test]
    fn reply_pending_index_is_present_on_fresh_and_25_upgrade() {
        for upgrade in [false, true] {
            let mut db = if upgrade {
                crate::storage::historical_memory(25)
            } else {
                let db = Connection::open_in_memory().unwrap();
                db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
                db
            };
            if upgrade {
                run(&mut db, 25, &|_, _| {}, &|| {}).unwrap();
            }
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                crate::storage::SCHEMA_VERSION
            );
            assert_eq!(
                crate::storage::SCHEMA_VERSION,
                crate::storage::MIGRATION_NAMES.last().unwrap().0,
                "current schema follows the complete migration registry"
            );
            assert_eq!(
                db.query_row(
                    "SELECT name FROM schema_migration WHERE version=40",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "0040_selection_nonappend_generation"
            );
            assert_eq!(
                db.query_row(
                    "SELECT name FROM schema_migration WHERE version=25",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "0025_history_attempt_index"
            );
            assert_eq!(
                db.query_row(
                    "SELECT name FROM schema_migration WHERE version=26",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "0026_reply_expected"
            );
            assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE type='index' AND name='history_tool_group_attempt'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
            let unforced:String=db.query_row("EXPLAIN QUERY PLAN SELECT rowid FROM receipt WHERE target_thread_id='t' AND json_extract(result_json,'$.reply.state') IN ('pending','awaiting_own_ask')",[],|row|row.get(3)).unwrap();
            eprintln!("PENDING_REPLY_UNFORCED_PLAN {unforced}");
            assert!(
                unforced.contains("SEARCH") && unforced.contains("target_thread_id=?"),
                "{unforced}"
            );
            let plan: String = db.query_row("EXPLAIN QUERY PLAN SELECT rowid FROM receipt INDEXED BY receipt_reply_pending WHERE target_thread_id='t' AND json_extract(result_json,'$.reply.state') IN ('pending','awaiting_own_ask')", [], |row| row.get(3)).unwrap();
            assert!(plan.contains("receipt_reply_pending"), "{plan}");
            let human_plan: String = db.query_row("EXPLAIN QUERY PLAN SELECT target_thread_id FROM receipt INDEXED BY receipt_human_reply_pending WHERE target_thread_id='t' AND json_extract(result_json,'$.reply.state')='pending' AND json_extract(result_json,'$.to_human')=1", [], |row| row.get(3)).unwrap();
            assert!(
                human_plan.contains("receipt_human_reply_pending"),
                "{human_plan}"
            );
        }
    }
    #[test]
    fn schema_37_upgrade_adds_human_index_without_rewriting_legacy_receipts() {
        use rusqlite::params;
        let mut db = crate::storage::historical_memory(37);
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('legacy','','','p','m','now')", []).unwrap();
        for index in 0..20 {
            let node = format!("legacy-node-{index}");
            db.execute(
                "INSERT INTO node(id,kind,created_at,payload_json) VALUES(?1,'message','old','{}')",
                [&node],
            )
            .unwrap();
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES(?1,?1,'{}','legacy',?2,'applied','old','{\"reply\":{\"state\":\"pending\"},\"to_human\":true}')", params![format!("legacy-receipt-{index}"), node]).unwrap();
        }
        run(&mut db, 37, &|_, _| {}, &|| {}).unwrap();
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            crate::storage::SCHEMA_VERSION
        );
        assert_eq!(
            crate::control::db::named_reply_counts(&db, "target_thread_id='legacy'")
                .get("pending")
                .copied(),
            Some(20)
        );
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE type='index' AND name='receipt_human_reply_pending'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
        let plan: String = db.query_row("EXPLAIN QUERY PLAN SELECT target_thread_id FROM receipt INDEXED BY receipt_human_reply_pending WHERE target_thread_id='legacy' AND json_extract(result_json,'$.reply.state')='pending' AND json_extract(result_json,'$.to_human')=1", [], |row| row.get(3)).unwrap();
        assert!(plan.contains("receipt_human_reply_pending"), "{plan}");
    }

    #[test]
    fn version_23_activity_indexes_include_malformed_shell_without_rewriting_it() {
        let db = crate::storage::historical_memory(22);
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now');").unwrap();
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','now'); PRAGMA ignore_check_constraints=ON; INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('bad','r','u','bad','exited','/','broken json','UTF-8','UTF-8','closed','now'),('good','r','u','good','exited','/','{\"spawned\":true}','UTF-8','UTF-8','closed','now'); PRAGMA ignore_check_constraints=OFF;").unwrap();
        db.execute_batch(include_str!("../../migrations/0023_activity_bounded.sql"))
            .unwrap();
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
            23
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

#[cfg(test)]
mod generated_summary_role_migration_tests {
    use super::run;
    use rusqlite::{Connection, params};
    use serde_json::{Value, json};

    const FORMAT: &str = "shellboy-summary-v1";
    type NegativeSnapshot = (String, Option<String>, i64, Option<String>, i64);

    fn node(db: &Connection, id: &str, kind: &str, payload: Option<&Value>, deleted: Option<&str>) {
        let raw = payload.map(Value::to_string);
        let bytes = raw.as_ref().map_or(0, |value| value.len() as i64);
        let hash = raw
            .as_ref()
            .map(|value| crate::digest::sha256_hex(value.as_bytes()));
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash,deleted_reason) VALUES(?1,?2,'now',?3,?4,?5,?6)",
            params![id, kind, raw, bytes, hash, deleted],
        ).unwrap();
    }

    fn replacement(
        db: &Connection,
        node_id: &str,
        id: &str,
        format: &str,
        state: &str,
        sources: &[&str],
    ) {
        db.execute(
            "INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES(?1,?2,'key',?3,'shellboy-summary-prompt-v3',?4,'now')",
            params![id, node_id, format, state],
        ).unwrap();
        for (ordinal, source_id) in sources.iter().enumerate() {
            db.execute(
                "INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision,source_hash) VALUES(?1,?2,?3,0,'source-hash')",
                params![id, ordinal as i64, source_id],
            ).unwrap();
        }
    }

    fn fixture_on(mut db: Connection) -> Connection {
        crate::storage::build_historical_schema(&mut db, 48);
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/a','p','m','now')", []).unwrap();
        node(
            &db,
            "source-a",
            "message",
            Some(&json!({"role":"user","content":"A"})),
            None,
        );
        node(
            &db,
            "source-b",
            "message",
            Some(&json!({"role":"user","content":"B"})),
            None,
        );

        let canonical = json!({"item":{"role":"developer","content":[{"type":"input_text","text":"summary body"}],"reasoning_epoch":4},"source_node_ids":["source-a","source-b"],"summary_format":FORMAT,"summary_usage":{"requests":1},"reasoning_epoch":4});
        node(&db, "positive", "summary", Some(&canonical), None);
        replacement(
            &db,
            "positive",
            "replacement-good",
            FORMAT,
            "completed",
            &["source-a", "source-b"],
        );

        let actual_developer = json!({"item":{"role":"developer","content":[{"type":"input_text","text":"instruction"}]},"source_node_ids":["source-a"],"summary_format":FORMAT});
        node(
            &db,
            "ordinary-developer",
            "message",
            Some(&actual_developer),
            None,
        );
        node(
            &db,
            "ordinary-kind-summary",
            "summary",
            Some(
                &json!({"item":{"role":"developer","content":[{"type":"input_text","text":"ordinary"}]},"source_node_ids":["source-a"]}),
            ),
            None,
        );
        node(
            &db,
            "already-user",
            "summary",
            Some(
                &json!({"item":{"role":"user","content":[{"type":"input_text","text":"already user"}]},"source_node_ids":["source-a"],"summary_format":FORMAT}),
            ),
            None,
        );
        replacement(
            &db,
            "already-user",
            "replacement-user",
            FORMAT,
            "completed",
            &["source-a"],
        );

        node(
            &db,
            "wrong-format",
            "summary",
            Some(
                &json!({"item":{"role":"developer","content":[{"type":"input_text","text":"wrong format"}]},"source_node_ids":["source-a"],"summary_format":"other"}),
            ),
            None,
        );
        replacement(
            &db,
            "wrong-format",
            "replacement-wrong-format",
            "other",
            "completed",
            &["source-a"],
        );
        node(
            &db,
            "pending",
            "summary",
            Some(
                &json!({"item":{"role":"developer","content":[{"type":"input_text","text":"pending"}]},"source_node_ids":["source-a"],"summary_format":FORMAT}),
            ),
            None,
        );
        replacement(
            &db,
            "pending",
            "replacement-pending",
            FORMAT,
            "running",
            &["source-a"],
        );
        node(
            &db,
            "no-replacement",
            "summary",
            Some(
                &json!({"item":{"role":"developer","content":[{"type":"input_text","text":"no replacement"}]},"source_node_ids":["source-a"],"summary_format":FORMAT}),
            ),
            None,
        );
        node(
            &db,
            "malformed",
            "summary",
            Some(
                &json!({"item":{"role":"developer","content":"not canonical"},"source_node_ids":["source-a"],"summary_format":FORMAT}),
            ),
            None,
        );
        replacement(
            &db,
            "malformed",
            "replacement-malformed",
            FORMAT,
            "completed",
            &["source-a"],
        );
        node(&db, "tombstone", "summary", None, Some("collected"));
        node(
            &db,
            "ambiguous",
            "summary",
            Some(
                &json!({"item":{"role":"developer","content":[{"type":"input_text","text":"ambiguous"}]},"source_node_ids":["source-a"],"summary_format":FORMAT}),
            ),
            None,
        );
        replacement(
            &db,
            "ambiguous",
            "replacement-ambiguous-a",
            FORMAT,
            "completed",
            &["source-a"],
        );
        replacement(
            &db,
            "ambiguous",
            "replacement-ambiguous-b",
            FORMAT,
            "completed",
            &["source-a"],
        );
        // Contradictory source ordering must not be normalized or guessed.
        node(
            &db,
            "source-mismatch",
            "summary",
            Some(
                &json!({"item":{"role":"developer","content":[{"type":"input_text","text":"mismatch"}]},"source_node_ids":["source-a","source-b"],"summary_format":FORMAT}),
            ),
            None,
        );
        replacement(
            &db,
            "source-mismatch",
            "replacement-source-mismatch",
            FORMAT,
            "completed",
            &["source-b", "source-a"],
        );
        let duplicate_raw = r#"{"item":{"role":"developer","role":"developer","content":[{"type":"input_text","text":"duplicate key"}]},"source_node_ids":["source-a"],"summary_format":"shellboy-summary-v1"}"#;
        let normalized: Value = serde_json::from_str(duplicate_raw).unwrap();
        let normalized_hash = crate::digest::sha256_hex(serde_json::to_vec(&normalized).unwrap());
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES('duplicate-key','summary','now',?1,?2,?3)",
            params![duplicate_raw, duplicate_raw.len() as i64, normalized_hash],
        ).unwrap();
        replacement(
            &db,
            "duplicate-key",
            "replacement-duplicate-key",
            FORMAT,
            "completed",
            &["source-a"],
        );
        db
    }

    fn fixture() -> Connection {
        fixture_on(Connection::open_in_memory().unwrap())
    }

    #[test]
    fn schema49_converts_only_positive_canonical_generated_summary_and_reopens_idempotently() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("schema49.sqlite");
        let mut db = fixture_on(Connection::open(&path).unwrap());
        let before_sources: Vec<(String,i64,String)> = db.prepare("SELECT source_node_id,ordinal,source_hash FROM replacement_source WHERE replacement_id='replacement-good' ORDER BY ordinal").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        let before_source_payloads: Vec<(String, String)> = db
            .prepare(
                "SELECT id,payload_json FROM node WHERE id IN ('source-a','source-b') ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let before_payload: String = db
            .query_row(
                "SELECT payload_json FROM node WHERE id='positive'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let negative_ids = [
            "ordinary-developer",
            "ordinary-kind-summary",
            "already-user",
            "wrong-format",
            "pending",
            "no-replacement",
            "malformed",
            "tombstone",
            "ambiguous",
            "source-mismatch",
            "duplicate-key",
        ];
        let negative_before: Vec<NegativeSnapshot> = negative_ids
            .iter()
            .map(|id| {
                let row = db.query_row(
                    "SELECT payload_json,payload_bytes,body_hash,payload_revision FROM node WHERE id=?1",
                    [id],
                    |r| Ok((r.get::<_,Option<String>>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,i64>(3)?)),
                ).unwrap();
                ((*id).to_owned(), row.0, row.1, row.2, row.3)
            })
            .collect();
        let before_usage: i64 = db
            .query_row(
                "SELECT logical_bytes FROM quota_counter WHERE category='logical.history'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        run(&mut db, 48, &|_, _| {}, &|| {}).unwrap();
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            super::super::SCHEMA_VERSION
        );
        let (payload,bytes,hash,revision):(String,i64,String,i64)=db.query_row("SELECT payload_json,payload_bytes,body_hash,payload_revision FROM node WHERE id='positive'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        let parsed: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["item"]["role"], "user");
        assert_eq!(parsed["item"]["content"][0]["text"], "summary body");
        assert_eq!(parsed["source_node_ids"], json!(["source-a", "source-b"]));
        assert_eq!(bytes, payload.len() as i64);
        assert_eq!(hash, crate::digest::sha256_hex(payload.as_bytes()));
        assert_eq!(revision, 1);
        for (id, raw, bytes, hash, revision) in negative_before {
            let after: (Option<String>, i64, Option<String>, i64) = db
                .query_row(
                    "SELECT payload_json,payload_bytes,body_hash,payload_revision FROM node WHERE id=?1",
                    [&id],
                    |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                after,
                (raw, bytes, hash, revision),
                "negative candidate changed: {id}"
            );
        }
        let after_sources: Vec<(String,i64,String)> = db.prepare("SELECT source_node_id,ordinal,source_hash FROM replacement_source WHERE replacement_id='replacement-good' ORDER BY ordinal").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(after_sources, before_sources);
        let after_source_payloads: Vec<(String, String)> = db
            .prepare(
                "SELECT id,payload_json FROM node WHERE id IN ('source-a','source-b') ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(after_source_payloads, before_source_payloads);
        let after_usage: i64 = db
            .query_row(
                "SELECT logical_bytes FROM quota_counter WHERE category='logical.history'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            after_usage,
            before_usage + payload.len() as i64 - before_payload.len() as i64
        );
        run(&mut db, super::super::SCHEMA_VERSION, &|_, _| {}, &|| {}).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT payload_revision FROM node WHERE id='positive'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        drop(db);
        for _ in 0..2 {
            let mut reopened = Connection::open(&path).unwrap();
            run(
                &mut reopened,
                super::super::SCHEMA_VERSION,
                &|_, _| {},
                &|| {},
            )
            .unwrap();
            let saved: String = reopened
                .query_row(
                    "SELECT payload_json FROM node WHERE id='positive'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(saved, payload);
            assert_eq!(
                reopened
                    .query_row(
                        "SELECT payload_revision FROM node WHERE id='positive'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
        }
    }

    #[test]
    fn schema49_payload_write_failure_rolls_back_role_ledger_version_and_accounting() {
        let mut db = fixture();
        let before: String = db
            .query_row(
                "SELECT payload_json FROM node WHERE id='positive'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let usage: i64 = db
            .query_row(
                "SELECT logical_bytes FROM quota_counter WHERE category='logical.history'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        db.execute_batch("CREATE TRIGGER reject_summary_role BEFORE UPDATE OF payload_json ON node WHEN OLD.id='positive' BEGIN SELECT RAISE(ABORT,'injected summary payload fault'); END;").unwrap();
        assert!(run(&mut db, 48, &|_, _| {}, &|| {}).is_err());
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            48
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM schema_migration WHERE version=49",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT payload_json FROM node WHERE id='positive'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            before
        );
        assert_eq!(
            db.query_row(
                "SELECT logical_bytes FROM quota_counter WHERE category='logical.history'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            usage
        );
    }
}

#[cfg(test)]
mod schedule_migration_tests {
    use super::*;

    fn schema50() -> Connection {
        let db = crate::storage::historical_memory(50);
        db.execute_batch("PRAGMA foreign_keys=ON;
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','completed','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','now');
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)
              VALUES('a','r','u','oa','exited','/','{}','utf-8','utf-8','closed','now'),('b','r','u','ob','stopped','/','{}','utf-8','utf-8','closed','now');
            INSERT INTO output_chunk VALUES('a',1,'stdout',0,X'00FF','now'),('b',1,'stderr',0,X'6162','now');").unwrap();
        assert_eq!(super::super::schema_version(&db).unwrap(), 50);
        db
    }

    fn saved_rows(db: &Connection) -> (Vec<String>, Vec<String>, Vec<String>, i64) {
        let rows = |sql: &str| {
            db.prepare(sql)
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        (
            rows(
                "SELECT json_array(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,retention_generation,exit_code,end_reason,output_complete,started_at,ended_at) FROM shell_execution ORDER BY id",
            ),
            rows(
                "SELECT json_array(execution_id,sequence,stream,stream_offset,hex(bytes),received_at) FROM output_chunk ORDER BY execution_id,sequence",
            ),
            rows(
                "SELECT sql FROM sqlite_schema WHERE type='index' AND tbl_name IN ('shell_execution','output_chunk') AND sql IS NOT NULL ORDER BY name",
            ),
            db.query_row(
                "SELECT logical_bytes FROM quota_counter WHERE category='logical.raw'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        )
    }

    #[test]
    fn schema51_preserves_shell_ids_output_and_indexes() {
        let mut db = schema50();
        let (shells, output, indexes, bytes) = saved_rows(&db);
        run(&mut db, 50, &|_, _| {}, &|| {}).unwrap();
        assert_eq!(validate_schema(&db).unwrap(), super::super::SCHEMA_VERSION);
        let (actual_shells, actual_output, actual_indexes, actual_bytes) = saved_rows(&db);
        assert_eq!(actual_shells, shells);
        assert_eq!(actual_output, output);
        assert_eq!(actual_bytes, bytes);
        assert!(indexes.iter().all(|index| actual_indexes.contains(index)));
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM shell_execution WHERE schedule_id IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn schema51_refuses_corrupt_shell_json_without_changing_database() {
        let mut db = schema50();
        db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE shell_execution SET shell_json='malformed' WHERE id='b'; PRAGMA ignore_check_constraints=OFF;").unwrap();
        let before = saved_rows(&db);
        let error = run(&mut db, 50, &|_, _| {}, &|| {}).unwrap_err();
        assert!(
            matches!(error,StorageError::Schema(message) if message=="migration 51 cannot rebuild shell execution \"b\": invalid shell_json")
        );
        assert_eq!(super::super::schema_version(&db).unwrap(), 50);
        assert_eq!(saved_rows(&db), before);
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('schedule','scheduled_shell_execution','scheduled_output_chunk')",[],|row|row.get::<_,i64>(0)).unwrap(),0);
        assert!(
            db.pragma_query_value(None, "foreign_keys", |row| row.get::<_, bool>(0))
                .unwrap()
        );
    }
}

#[cfg(test)]
mod unified_attempt_purpose_tests {
    use super::*;
    use crate::{protocol::UsageGetArgs, usage_totals::breakdown};
    use serde_json::json;

    fn schema51_with_summary() -> (Connection, serde_json::Value) {
        let db = crate::storage::historical_memory(51);
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('thread','/','','p','m','now');").unwrap();
        let number = |known_sum, unknown_count| json!({"known_sum":known_sum,"unknown_count":unknown_count,"overflow":false});
        let ledger = json!({"id":"compaction", "thread_id":"thread", "turn_id":null, "run_id":null,
            "profile_id":"p", "model_id":"m", "started_at":"2026-10-31T00:00:00Z", "updated_at":"2026-11-01T00:00:00Z",
            "requests":3, "input_tokens":number(Some(70),1), "cached_tokens":number(Some(0),2),
            "cache_creation_tokens":number(None,3), "output_tokens":number(Some(5),0),
            "rate_observations":[{"status":429,"applied_wait_ms":100},null,{"status":200,"applied_wait_ms":0}]});
        db.execute(
            "INSERT INTO app_meta(key,value) VALUES('summary.usage.thread.compaction',?1)",
            [ledger.to_string()],
        )
        .unwrap();
        (db, ledger)
    }

    #[test]
    fn unrepresentable_summary_ledger_rolls_back_prior_rows_and_schema() {
        for invalid in ["overflow", "above_signed_bound", "run_without_turn"] {
            let (mut db, mut ledger) = schema51_with_summary();
            ledger["id"] = json!("z-invalid");
            match invalid {
                "overflow" => ledger["input_tokens"]["overflow"] = json!(true),
                "above_signed_bound" => {
                    ledger["input_tokens"]["known_sum"] = json!(i64::MAX as u64 + 1)
                }
                _ => ledger["run_id"] = json!("missing-turn"),
            }
            db.execute(
                "INSERT INTO app_meta(key,value) VALUES('summary.usage.thread.z-invalid',?1)",
                [ledger.to_string()],
            )
            .unwrap();
            let error = run(&mut db, 51, &|_, _| {}, &|| {}).unwrap_err();
            assert!(
                matches!(error, StorageError::Schema(message) if message.contains("summary.usage.thread.z-invalid")),
                "{invalid}"
            );
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                51
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key LIKE 'summary.usage.%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                2
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM pragma_table_info('model_attempt') WHERE name='kind'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
        }
    }

    #[test]
    fn summary_ledger_migration_preserves_breakdown() {
        let (mut db, ledger) = schema51_with_summary();
        run(&mut db, 51, &|_, _| {}, &|| {}).unwrap();
        let usage = breakdown::read(
            &db,
            &UsageGetArgs {
                group_by: Some("day".into()),
                timezone: Some("UTC".into()),
                since: Some("2026-10-31T00:00:00Z".into()),
                until: Some("2026-11-01T00:00:00Z".into()),
                ..Default::default()
            },
            "fixture",
        )
        .unwrap();
        let item = &usage["items"][0];
        assert_eq!(item["key"], "2026-10-31");
        assert_eq!(item["compaction"]["compactions"], 1);
        assert_eq!(item["compaction"]["requests"], 3);
        for field in [
            "input_tokens",
            "cached_tokens",
            "cache_creation_tokens",
            "output_tokens",
        ] {
            assert_eq!(item["compaction"][field], ledger[field], "{field}");
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM app_meta WHERE key LIKE 'summary.usage.%'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM model_attempt WHERE kind='summary' AND turn_id IS NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            3
        );
    }
}

#[cfg(test)]
mod thread_creation_migration_tests {
    use super::*;

    #[test]
    fn schema54_backfills_earliest_owned_activity_without_inherited_path_or_updates() {
        let mut db = crate::storage::historical_memory(53);
        db.pragma_update(None, "foreign_keys", true).unwrap();
        db.execute_batch("
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES
                ('t','/','','p','m','99'),('fork','/','','p','m','00'),('held','/','','p','m','99');
            INSERT INTO schedule(id,thread_id,once_at,enabled,cwd,command) VALUES('saved-schedule','t',1,1,'/','echo test');
            INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)
                VALUES('saved-shell','saved-schedule','saved-op','exited','/','{}','utf-8','utf-8','closed','now');
            INSERT INTO output_chunk VALUES('saved-shell',1,'stdout',0,X'4142','now');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','completed','41');
            INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','40');
            INSERT INTO node(id,turn_id,kind,created_at) VALUES('known','u','message','20'),('held-node',NULL,'message','10');
            INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)
                VALUES('receipt','op','{}','held','held-node','held','30','{}');
            INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('fork',0,'known');").unwrap();
        run(&mut db, 53, &|_, _| {}, &|| {}).unwrap();
        let (kind, output): (String, Vec<u8>) = db.query_row(
            "SELECT s.kind,o.bytes FROM schedule s JOIN shell_execution e ON e.schedule_id=s.id JOIN output_chunk o ON o.execution_id=e.id WHERE s.id='saved-schedule'",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(kind, "time");
        assert_eq!(output, b"AB");
        assert!(
            db.pragma_query_value(None, "foreign_keys", |r| r.get::<_, bool>(0))
                .unwrap()
        );
        for (id, expected) in [("t", Some("20")), ("held", Some("10")), ("fork", None)] {
            let actual: Option<String> = db
                .query_row("SELECT created_at FROM thread WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(actual.as_deref(), expected, "{id}");
        }
    }
}

#[cfg(test)]
mod automatic_compaction_migration_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn automatic_compaction_migration_preserves_known_list_facts() {
        let mut db = crate::storage::historical_memory(54);
        db.execute_batch("
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','completed','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','now');").unwrap();
        let discard = json!({"phase":"closed_summary","elapsed_ms":3,"outcome":{"kind":"rejected","reason":"run_started"}});
        let provider = json!({"error":{"kind":"provider","message":"provider text containing automatic_trim_attempt"}});
        let typed = json!({"code":"invalid_state","message":"original path retained","details":{"source_nodes":2}});
        let midturn = json!({"scope":"active_turn","reason":"open_tool_loop"});
        for (id, body) in [
            ("adopted",json!({"automatic_trim_attempt":{"token":"tok","outcome":"adopted","summary":{"source_nodes":8},"committed_stages":{"closed_summary":{"source_nodes":8}}},"automatic_trim":{"source_nodes":99}}).to_string()),
            ("unknown",json!({"automatic_trim_attempt":{"token":"tok","outcome":"unknown","compaction_outcome":discard},"automatic_trim":{"source_nodes":99}}).to_string()),
            ("recorded",json!({"automatic_trim":{"source_nodes":4}}).to_string()),
            ("overflow",json!({"automatic_trim_attempt":{"outcome":"rejected","error":typed,"midturn":midturn,"maximum_outcome":"no_safe_reduction"}}).to_string()),
            ("broken","{broken".into()),
            ("provider",provider.to_string()),
        ] {
            db.execute("INSERT INTO model_attempt(id,kind,thread_id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at) VALUES(?1,'conversation','t','u',0,0,'p','m',0,'completed','{}',?2,'2026-01-01T00:00:00Z')", rusqlite::params![id,body]).unwrap();
        }
        // Frozen schema-54 compact.list items for this fixture. Keep the legacy
        // projection out of the runtime reader.
        let legacy_item = |id: &str, state: &str, source_nodes| {
            json!({
                "event_id": format!("attempt:{id}"), "thread_id": "t",
                "kind": "automatic_reduction", "reason": "failure",
                "at": "2026-01-01T00:00:00Z",
                "timestamp_basis": "replacement_created_at_or_attempt_ended_or_started_at",
                "attempt_id": id, "turn_id": "u", "state": state,
                "trigger": {"failure_kind": null, "retry_number": null,
                    "retained_failures_in_turn_through_attempt": 0},
                "metrics": {"source_nodes": source_nodes, "reason": "failure",
                    "before_characters": null, "after_characters": null,
                    "omitted_nodes": null, "marker_node_id": null,
                    "summary_characters": null, "summary_limit": null,
                    "source_characters": null, "model_requests": null,
                    "spilled_groups": null, "summary_node_id": null,
                    "summary_rejected": null, "no_target": null,
                    "summary_usage": null, "first_node_id": null, "last_node_id": null,
                    "after_current_request_nodes": null},
                "post_compaction_attempt_id": null, "post_compaction_input_tokens": null
            })
        };
        let mut unknown = legacy_item("unknown", "unknown", json!(null));
        unknown["compaction_outcome"] = discard.clone();
        let legacy_items = json!([
            unknown,
            legacy_item("recorded", "recorded", json!(4)),
            legacy_item("overflow", "rejected", json!(2)),
            legacy_item("adopted", "adopted", json!(8)),
        ]);
        run(&mut db, 54, &|_, _| {}, &|| {}).unwrap();
        let args = crate::protocol::CompactListArgs {
            thread_id: Some("t".into()),
            since: None,
            until: None,
            limit: 20,
        };
        let page = crate::readback::compactions(&db, "t", &args).unwrap();
        assert_eq!(page["items"], legacy_items);
        assert_eq!(page["more"], false);
        let envelope = |id| {
            let text: String = db
                .query_row(
                    "SELECT result_json FROM automatic_compaction WHERE attempt_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            serde_json::from_str::<serde_json::Value>(&text).unwrap()
        };
        assert_eq!(
            envelope("overflow"),
            json!({"metrics":{"source_nodes":2},"error":typed,"midturn":midturn,"maximum_outcome":"no_safe_reduction"})
        );
        assert_eq!(envelope("recorded"), json!({"metrics":{"source_nodes":4}}));
        assert_eq!(envelope("unknown"), json!({"metrics":null}));
        let error = |id| {
            db.query_row(
                "SELECT error_json FROM model_attempt WHERE id=?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap()
        };
        assert_eq!(error("adopted"), None);
        assert_eq!(error("broken").as_deref(), Some("{broken"));
        assert_eq!(error("provider"), Some(provider.to_string()));
        assert!(db.query_row("SELECT json_extract(closed_summary,'$.source_nodes')=8 AND active_summary IS NULL FROM automatic_compaction WHERE attempt_id='adopted'",[],|r|r.get::<_,bool>(0)).unwrap());
    }
}
