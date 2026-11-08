//! Startup-only schema upgrades. Runtime readers use the current schema only.
use super::{StorageError, migration_integrity::Impact, validate_schema};
use rusqlite::Connection;

pub(super) struct Migration {
    pub version: i64,
    pub sql: &'static str,
    pub foreign_keys: Option<Impact>,
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
        sql: include_str!("../../migrations/0038_independent_runs.sql"),
        foreign_keys: None,
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
        // Rebuild the Run table without changing referent IDs or dependent rows.
        if migration.version == 38 {
            connection.pragma_update(None, "foreign_keys", false)?;
        }
        let migrated = (|| -> Result<(), StorageError> {
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
            if migration.version == 4 {
                migrate_attachment_sources(&transaction)?;
            }
            transaction.commit()?;
            Ok(())
        })();
        if migration.version == 38 {
            connection.pragma_update(None, "foreign_keys", true)?;
        }
        migrated?;
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
    fn failed_run_rebuild_rolls_back_and_restores_foreign_keys() {
        let mut db=Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.pragma_update(None,"foreign_keys",true).unwrap();
        // Inject a collision after FK checks are disabled, before any Run copy.
        db.execute_batch("CREATE TABLE independent_run(collision INTEGER); DELETE FROM schema_migration WHERE version=38; PRAGMA user_version=37;").unwrap();
        assert!(run(&mut db,37,&|_,_|{},&||{}).is_err());
        assert_eq!(db.query_row("PRAGMA foreign_keys",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0)).unwrap(),37);
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='run'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }

    #[test]
    fn recovery_index_upgrades_from_each_recent_schema() {
        for start in [24, 25, 26, 27, 28] {
            let mut db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch("DROP INDEX jobs_running_recovery; DROP INDEX IF EXISTS model_attempt_active_recovery; DELETE FROM schema_migration WHERE version>=29; ALTER TABLE thread DROP COLUMN auto_compact_at; ALTER TABLE thread DROP COLUMN segment_chars; ALTER TABLE thread DROP COLUMN reduce_rounds; DROP INDEX IF EXISTS threads_visible_updated; DROP INDEX IF EXISTS threads_archived_updated; DROP TABLE thread_tag; DROP INDEX threads_cwd_updated; DROP TRIGGER logical_index_embedding_chunk_insert; DROP TRIGGER logical_index_embedding_chunk_delete; DROP TRIGGER logical_index_embedding_chunk_update; DROP TRIGGER logical_index_space_insert; DROP TRIGGER logical_index_space_delete; DROP TRIGGER logical_index_space_update; DROP INDEX chunks_job; ALTER TABLE index_space DROP COLUMN int8_scale; ALTER TABLE thread DROP COLUMN branch_kind; ALTER TABLE thread DROP COLUMN archived_at;")
                .unwrap();
            if start < 28 {
                db.execute_batch("DROP TRIGGER tool_group_recovery_group_insert; DROP TRIGGER tool_group_recovery_group_update; DROP TRIGGER tool_group_recovery_group_delete; DROP TRIGGER tool_group_recovery_call_insert; DROP TRIGGER tool_group_recovery_call_update; DROP TRIGGER tool_group_recovery_call_delete; DROP TRIGGER tool_group_recovery_result_insert; DROP TRIGGER tool_group_recovery_result_update; DROP TRIGGER tool_group_recovery_result_delete; DROP TABLE tool_group_recovery_candidate;")
                    .unwrap();
            }
            if start < 26 {
                db.execute_batch("DROP INDEX receipt_reply_pending;")
                    .unwrap();
            }
            if start < 25 {
                db.execute_batch("DROP INDEX history_tool_group_attempt;")
                    .unwrap();
            }
            db.execute("DELETE FROM schema_migration WHERE version>?1", [start])
                .unwrap();
            db.pragma_update(None, "user_version", start).unwrap();
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
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("DROP INDEX model_attempt_active_recovery; DELETE FROM schema_migration WHERE version>=29; ALTER TABLE thread DROP COLUMN auto_compact_at; ALTER TABLE thread DROP COLUMN segment_chars; ALTER TABLE thread DROP COLUMN reduce_rounds; DROP INDEX IF EXISTS threads_visible_updated; DROP INDEX IF EXISTS threads_archived_updated; DROP TABLE thread_tag; DROP INDEX threads_cwd_updated; DROP TRIGGER logical_index_embedding_chunk_insert; DROP TRIGGER logical_index_embedding_chunk_delete; DROP TRIGGER logical_index_embedding_chunk_update; DROP TRIGGER logical_index_space_insert; DROP TRIGGER logical_index_space_delete; DROP TRIGGER logical_index_space_update; DROP INDEX chunks_job; ALTER TABLE index_space DROP COLUMN int8_scale; ALTER TABLE thread DROP COLUMN branch_kind; ALTER TABLE thread DROP COLUMN archived_at; PRAGMA user_version=28;
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
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("DROP TRIGGER tool_group_recovery_group_insert; DROP TRIGGER tool_group_recovery_group_update; DROP TRIGGER tool_group_recovery_group_delete; DROP TRIGGER tool_group_recovery_call_insert; DROP TRIGGER tool_group_recovery_call_update; DROP TRIGGER tool_group_recovery_call_delete; DROP TRIGGER tool_group_recovery_result_insert; DROP TRIGGER tool_group_recovery_result_update; DROP TRIGGER tool_group_recovery_result_delete; DROP TABLE tool_group_recovery_candidate; DROP INDEX model_attempt_active_recovery; ALTER TABLE thread DROP COLUMN auto_compact_at; ALTER TABLE thread DROP COLUMN segment_chars; ALTER TABLE thread DROP COLUMN reduce_rounds; DROP INDEX IF EXISTS threads_visible_updated; DROP INDEX IF EXISTS threads_archived_updated; DROP TABLE thread_tag; DROP INDEX threads_cwd_updated; DROP TRIGGER logical_index_embedding_chunk_insert; DROP TRIGGER logical_index_embedding_chunk_delete; DROP TRIGGER logical_index_embedding_chunk_update; DROP TRIGGER logical_index_space_insert; DROP TRIGGER logical_index_space_delete; DROP TRIGGER logical_index_space_update; DROP INDEX chunks_job; ALTER TABLE index_space DROP COLUMN int8_scale; ALTER TABLE thread DROP COLUMN branch_kind; ALTER TABLE thread DROP COLUMN archived_at; DELETE FROM schema_migration WHERE version>=28; PRAGMA user_version=27;
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
            let mut db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            if upgrade {
                db.execute_batch("DROP INDEX receipt_reply_pending; DROP INDEX jobs_running_recovery; DROP INDEX model_attempt_active_recovery; ALTER TABLE thread DROP COLUMN auto_compact_at; ALTER TABLE thread DROP COLUMN segment_chars; ALTER TABLE thread DROP COLUMN reduce_rounds; DROP INDEX IF EXISTS threads_visible_updated; DROP INDEX IF EXISTS threads_archived_updated; DROP TABLE thread_tag; DROP INDEX threads_cwd_updated; DROP TRIGGER logical_index_embedding_chunk_insert; DROP TRIGGER logical_index_embedding_chunk_delete; DROP TRIGGER logical_index_embedding_chunk_update; DROP TRIGGER logical_index_space_insert; DROP TRIGGER logical_index_space_delete; DROP TRIGGER logical_index_space_update; DROP INDEX chunks_job; ALTER TABLE index_space DROP COLUMN int8_scale; ALTER TABLE thread DROP COLUMN branch_kind; ALTER TABLE thread DROP COLUMN archived_at; DELETE FROM schema_migration WHERE version>=26; PRAGMA user_version=25;").unwrap();
                run(&mut db, 25, &|_, _| {}, &|| {}).unwrap();
            }
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                crate::storage::SCHEMA_VERSION
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
            let plan: String = db.query_row("EXPLAIN QUERY PLAN SELECT rowid FROM receipt WHERE target_thread_id='t' AND json_extract(result_json,'$.reply.state')='pending'", [], |row| row.get(3)).unwrap();
            assert!(plan.contains("receipt_reply_pending"), "{plan}");
        }
    }
    #[test]
    fn version_22_upgrade_preserves_rows_and_installs_activity_indexes() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("DROP INDEX history_tool_group_attempt; DROP INDEX IF EXISTS receipt_reply_pending; DROP INDEX jobs_running_recovery; DROP INDEX activity_attempt_turn; DROP INDEX activity_turn_run; DROP INDEX activity_turn_run_state; DROP INDEX activity_group_turn_state; DROP INDEX activity_shell_spawned_turn; DROP INDEX activity_shell_uncollected_owner; DROP INDEX receipt_run_completion; DROP INDEX model_attempt_active_recovery; ALTER TABLE thread DROP COLUMN auto_compact_at; ALTER TABLE thread DROP COLUMN segment_chars; ALTER TABLE thread DROP COLUMN reduce_rounds; DROP INDEX IF EXISTS threads_visible_updated; DROP INDEX IF EXISTS threads_archived_updated; DROP TABLE thread_tag; DROP INDEX threads_cwd_updated; DROP TRIGGER logical_index_embedding_chunk_insert; DROP TRIGGER logical_index_embedding_chunk_delete; DROP TRIGGER logical_index_embedding_chunk_update; DROP TRIGGER logical_index_space_insert; DROP TRIGGER logical_index_space_delete; DROP TRIGGER logical_index_space_update; DROP INDEX chunks_job; ALTER TABLE index_space DROP COLUMN int8_scale; ALTER TABLE thread DROP COLUMN branch_kind; ALTER TABLE thread DROP COLUMN archived_at; DELETE FROM schema_migration WHERE version>=23; PRAGMA user_version=22; INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now');").unwrap();
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','now'); PRAGMA ignore_check_constraints=ON; INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('bad','r','u','bad','exited','/','broken json','UTF-8','UTF-8','closed','now'),('good','r','u','good','exited','/','{\"spawned\":true}','UTF-8','UTF-8','closed','now'); PRAGMA ignore_check_constraints=OFF;").unwrap();
        let observed = std::cell::RefCell::new(Vec::new());
        run(
            &mut db,
            22,
            &|current, total| observed.borrow_mut().push((current, total)),
            &|| {},
        )
        .unwrap();
        let total = super::super::SCHEMA_VERSION - 22;
        assert_eq!(
            *observed.borrow(),
            (1..=total).map(|n| (n, total)).collect::<Vec<_>>()
        );
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
            "history_tool_group_attempt",
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
