//! Empty forward-built historical databases and the pinned schema-42 baseline.

use rusqlite::{Connection, TransactionBehavior};
use std::path::Path;

/// Open the exact pinned schema-42 database used by historical upgrade tests.
pub(crate) fn open_schema42_fixture(path: &Path) -> Connection {
    let db = Connection::open(path).expect("open schema-42 fixture database");
    db.execute_batch(include_str!("../fixtures/storage-current-schema42.sql"))
        .expect("load pinned schema-42 fixture");
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        42,
        "pinned historical fixture must end at schema 42"
    );
    db
}

const THROUGH_54: &[&str] = &[
    include_str!("../../migrations/0001_initial.sql"),
    include_str!("../../migrations/0002_gc_indexes.sql"),
    include_str!("../../migrations/0003_context_indexes.sql"),
    include_str!("../../migrations/0004_attachment_provenance.sql"),
    include_str!("../../migrations/0005_origin_credentials.sql"),
    include_str!("../../migrations/0006_receipt_budget.sql"),
    include_str!("../../migrations/0007_usage_and_source_indexes.sql"),
    include_str!("../../migrations/0008_receipt_accounting_compat.sql"),
    include_str!("../../migrations/0009_compaction_turn_indexes.sql"),
    include_str!("../../migrations/0010_hot_query_indexes.sql"),
    include_str!("../../migrations/0011_current_index_metadata.sql"),
    include_str!("../../migrations/0012_revisit_metadata.sql"),
    include_str!("../../migrations/0013_message_path_admission.sql"),
    include_str!("../../migrations/0014_turn_interrupt.sql"),
    include_str!("../../migrations/0015_readback_indexes.sql"),
    include_str!("../../migrations/0016_thread_observation.sql"),
    include_str!("../../migrations/0017_thread_parent.sql"),
    include_str!("../../migrations/0018_summary_usage_metadata.sql"),
    include_str!("../../migrations/0019_context_attempt_scope.sql"),
    include_str!("../../migrations/0020_gc_index_source.sql"),
    include_str!("../../migrations/0021_event_retention.sql"),
    include_str!("../../migrations/0022_logical_usage_counters.sql"),
    include_str!("../../migrations/0023_activity_bounded.sql"),
    include_str!("../../migrations/0024_revisit_messages.sql"),
    include_str!("../../migrations/0025_history_attempt_index.sql"),
    include_str!("../../migrations/0026_reply_expected.sql"),
    include_str!("../../migrations/0027_index_recovery.sql"),
    include_str!("../../migrations/0028_tool_group_recovery_candidates.sql"),
    include_str!("../../migrations/0029_active_attempt_recovery_index.sql"),
    include_str!("../../migrations/0030_thread_auto_compact_at.sql"),
    include_str!("../../migrations/0031_thread_summary_parameters.sql"),
    include_str!("../../migrations/0032_thread_archive.sql"),
    include_str!("../../migrations/0033_index_int8_scale.sql"),
    include_str!("../../migrations/0034_thread_tags.sql"),
    include_str!("../../migrations/0035_index_reclaim_lookup.sql"),
    include_str!("../../migrations/0036_spill_tool_metadata.sql"),
    include_str!("../../migrations/0037_thread_branch_kind.sql"),
    include_str!("../../migrations/0038_human_reply_pending.sql"),
    include_str!("../../migrations/0039_independent_runs.sql"),
    include_str!("../../migrations/0040_selection_nonappend_generation.sql"),
    include_str!("../../migrations/0041_receipt_reply_node_lookup.sql"),
    include_str!("../../migrations/0042_notification_semantic_descriptor.sql"),
    include_str!("../../migrations/0043_thread_single_summary_settings.sql"),
    include_str!("../../migrations/0044_thread_max_tokens.sql"),
    include_str!("../../migrations/0045_thread_spill_override.sql"),
    include_str!("../../migrations/0046_scope_identity_v2.sql"),
    include_str!("../../migrations/0047_drop_selection_nonappend_generation.sql"),
    include_str!("../../migrations/0048_human_question_path_admission.sql"),
    include_str!("../../migrations/0049_generated_summary_user_role.sql"),
    include_str!("../../migrations/0050_attachment_page_order.sql"),
    include_str!("../../migrations/0051_thread_schedule.sql"),
    include_str!("../../migrations/0052_unified_model_attempt.sql"),
    include_str!("../../migrations/0053_retired_schedule_shells.sql"),
    include_str!("../../migrations/0054_thread_created_at.sql"),
];

/// Build an empty legacy database, independently of the current bootstrap.
/// The Rust row-conversion hooks have no input here; migration 52 also has DDL.
pub(crate) fn build_historical_schema(db: &mut Connection, version: usize) {
    assert!((1..=THROUGH_54.len()).contains(&version));
    db.pragma_update(None, "foreign_keys", true).unwrap();
    for (index, sql) in THROUGH_54[..version].iter().enumerate() {
        let rebuild = sql.starts_with("-- rebuild: foreign keys off\n");
        if rebuild {
            db.pragma_update(None, "foreign_keys", false).unwrap();
        }
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        tx.execute_batch(sql)
            .unwrap_or_else(|e| panic!("build schema {}: {e}", index + 1));
        if index + 1 == 52 {
            tx.execute_batch(include_str!(
                "../../src/storage/migrate/retire_summary_ledger.sql"
            ))
            .unwrap();
        }
        if rebuild {
            assert!(
                !tx.prepare("PRAGMA foreign_key_check")
                    .unwrap()
                    .exists([])
                    .unwrap()
            );
        }
        tx.commit().unwrap();
        if rebuild {
            db.pragma_update(None, "foreign_keys", true).unwrap();
        }
    }
    assert_eq!(
        db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        version as i64
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM schema_migration", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        version as i64
    );
}
