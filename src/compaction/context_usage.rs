//! Saved context usage shared by canonical context metrics and display reads.
//! Caller owns the connection; JSON preparation follows the read transaction.
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
    usage::{self, StandardUsage},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

mod bounded;
pub(crate) type Scope = (String, String, String);
struct Coverage {
    latest_complete: bool,
    pending_complete: bool,
}
pub(crate) struct RawContextUsage {
    scope: Scope,
    latest: Option<RawSuccess>,
    coverage: Option<Coverage>,
}
struct RawSuccess {
    attempt_id: String,
    usage: StandardUsage,
    started_at: String,
    ended_at: Option<String>,
    next_request_pending: bool,
}
impl RawContextUsage {
    /// Proactive-compaction evidence from the same latest completed attempt the
    /// CLI reports: its ID, provider-reported input tokens (never estimated),
    /// and whether a replacement was adopted after it started.
    pub(crate) fn latest_reported_input(&self) -> Option<(&str, Option<u64>, bool)> {
        if self.coverage.is_some() {
            return None;
        }
        self.latest.as_ref().map(|latest| {
            (
                latest.attempt_id.as_str(),
                latest.usage.input_tokens,
                latest.next_request_pending,
            )
        })
    }
}
impl RawContextUsage {
    /// Provider-reported usage of the same latest attempt; all-unknown when none.
    pub(crate) fn reported_usage(&self) -> StandardUsage {
        self.latest.as_ref().map(|l| l.usage).unwrap_or_default()
    }
}
#[cfg(test)]
impl RawContextUsage {
    fn scope(&self) -> (&str, &str, &str) {
        (&self.scope.0, &self.scope.1, &self.scope.2)
    }
}
fn scalar_limit() -> StorageError {
    ApiError::new(
        ErrorCode::InputLimit,
        "Saved context-usage scalar exceeds display limits",
    )
    .into()
}
fn latest_query() -> String {
    format!("SELECT
        CASE WHEN octet_length(a.id)<=?2 THEN a.id END, {},
        CASE WHEN octet_length(a.started_at)<=?2 THEN a.started_at END,
        CASE WHEN octet_length(a.ended_at)<=?2 THEN a.ended_at END,
        EXISTS(SELECT 1 FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id
          WHERE tr.thread_id=t.id AND r.created_at>a.started_at),
        COALESCE(octet_length(a.id)>?2 OR octet_length(a.started_at)>?2
          OR COALESCE(octet_length(a.ended_at)>?2,0),0)
        FROM thread t JOIN turn u ON u.thread_id=t.id JOIN model_attempt a ON a.turn_id=u.id
        WHERE t.id=?1 AND a.state='completed' AND a.profile_id=t.profile_id AND a.model_id=t.model_id
        ORDER BY a.rowid DESC LIMIT 1", usage::sql_projection("a.usage_json"))
}

/// None preserves the canonical CLI scalar contract. Display callers pass512.
/// Display reads use finite native prefixes and explicit incomplete evidence.
pub(crate) fn read_context_usage_tx(
    db: &Connection,
    thread: &str,
    scalar_bytes: Option<usize>,
) -> Result<RawContextUsage, StorageError> {
    if let Some(bytes) = scalar_bytes {
        if bytes == 0 || bytes > 512 {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Context-usage display scalar bound must be1..512 bytes",
            )
            .into());
        }
        if thread.len() > bytes {
            return Err(scalar_limit());
        }
    }
    let limit = scalar_bytes.map(|bytes| bytes as i64).unwrap_or(i64::MAX);
    let scope: Option<(Option<String>, Option<String>)> = db
        .query_row(
            "SELECT CASE WHEN octet_length(profile_id)<=?2 THEN profile_id END,
                CASE WHEN octet_length(model_id)<=?2 THEN model_id END
         FROM thread WHERE id=?1 AND deleted_at IS NULL",
            params![thread, limit],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((profile, model)) = scope else {
        return Err(ApiError::new(
            ErrorCode::ThreadNotFound,
            "Context target Thread is unavailable",
        )
        .into());
    };
    let (Some(profile), Some(model)) = (profile, model) else {
        return Err(scalar_limit());
    };
    if scalar_bytes.is_some() {
        return bounded::read(db, thread, limit, (thread.into(), profile, model));
    }
    // The exact saved rowid ordering and replacement timestamp semantics are
    // deliberately shared with the CLI. A newer unknown usage never falls back.
    let sql = latest_query();
    let row = db
        .query_row(&sql, params![thread, limit], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                StandardUsage::read(row, 1)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, bool>(8)?,
                row.get::<_, bool>(9)?,
            ))
        })
        .optional()?;
    let latest = match row {
        None => None,
        Some((_, _, _, _, _, true)) => return Err(scalar_limit()),
        Some((
            Some(attempt_id),
            usage,
            Some(started_at),
            ended_at,
            next_request_pending,
            false,
        )) => Some(RawSuccess {
            attempt_id,
            usage,
            started_at,
            ended_at,
            next_request_pending,
        }),
        Some(_) => return Err(scalar_limit()),
    };
    Ok(RawContextUsage {
        scope: (thread.into(), profile, model),
        latest,
        coverage: None,
    })
}
/// The captured scope is private evidence for settings-aware display adoption.
/// It never adds a field to the canonical CLI result.
pub(crate) fn prepare_context_usage(raw: RawContextUsage) -> (Value, Scope) {
    let latest = raw.latest.map(|latest| {
        json!({
            "attempt_id": latest.attempt_id,
            "input_tokens": latest.usage.input_tokens,
            "cached_tokens": latest.usage.cached_tokens,
            "started_at": latest.started_at,
            "ended_at": latest.ended_at,
            "next_request_pending": latest.next_request_pending
        })
    });
    if let Some(coverage) = raw.coverage
        && (!coverage.latest_complete || !coverage.pending_complete)
    {
        let mut candidate = latest;
        if let Some(value) = candidate.as_mut()
            && (!coverage.latest_complete
                || (!coverage.pending_complete && value["next_request_pending"] != true))
        {
            value["next_request_pending"] = Value::Null;
        }
        return (
            json!({
                "last_successful_attempt": if coverage.latest_complete {candidate.clone()} else {None},
                "bounded_observation": {
                    "candidate": if coverage.latest_complete {None} else {candidate},
                    "latest_complete":coverage.latest_complete,
                    "replacement_coverage_complete":coverage.pending_complete,
                    "notice":"Finite retained prefixes; unknown is not no saved usage or no pending edit"
                }
            }),
            raw.scope,
        );
    }
    (json!({"last_successful_attempt": latest}), raw.scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compaction::Compactor, config::Config, storage::DbWorker};
    use std::sync::Arc;
    async fn fixture() -> (tempfile::TempDir, DbWorker, Compactor) {
        let root = tempfile::tempdir().unwrap();
        let db = DbWorker::open(root.path().join("metrics.db"))
            .await
            .unwrap();
        db.call(|db|{db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','Synthetic','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');")?;Ok(())}).await.unwrap();
        let compactor = Compactor::new(db.clone(), Arc::new(Config::default()));
        (root, db, compactor)
    }
    async fn attempt(
        db: &DbWorker,
        id: &str,
        state: &str,
        profile: &str,
        model: &str,
        usage: Option<String>,
        started: &str,
    ) {
        let (id, state, profile, model, started) = (
            id.to_owned(),
            state.to_owned(),
            profile.to_owned(),
            model.to_owned(),
            started.to_owned(),
        );
        db.call(move|db|{db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES(?1,'u',0,0,?2,?3,0,?4,'{}',?5,?6,NULL)",rusqlite::params![id,profile,model,state,usage,started])?;Ok(())}).await.unwrap();
    }
    #[tokio::test]
    async fn canonical_and_shared_transaction_agree_on_absence_latest_matching_scope_and_pending_edit()
     {
        let (_root, db, compactor) = fixture().await;
        assert_eq!(
            compactor.context_metrics("t").await.unwrap(),
            json!({"last_successful_attempt":null})
        );
        attempt(
            &db,
            "first",
            "completed",
            "p",
            "m",
            Some(json!({"input_tokens":9,"input_tokens_details":{"cached_tokens":2}}).to_string()),
            "2026-01-01T00:00:00Z",
        )
        .await;
        // Later rowid wins, not started_at. Other model/profile/noncompleted
        // attempts do not replace the matching completed witness.
        attempt(
            &db,
            "newest",
            "completed",
            "p",
            "m",
            Some(json!({"input_tokens":5,"input_tokens_details":{"cached_tokens":1}}).to_string()),
            "2020-01-01T00:00:00Z",
        )
        .await;
        attempt(
            &db,
            "other-model",
            "completed",
            "p",
            "other",
            Some("{}".into()),
            "later",
        )
        .await;
        attempt(
            &db,
            "other-profile",
            "completed",
            "other",
            "m",
            Some("{}".into()),
            "later",
        )
        .await;
        attempt(
            &db,
            "not-completed",
            "failed",
            "p",
            "m",
            Some("{}".into()),
            "later",
        )
        .await;
        db.call(|db|{db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('marker','summary','now','{}'); INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES('edit','marker','key','format','prompt','completed','2021-01-01T00:00:00Z'); INSERT INTO thread_replacement VALUES('t','edit');")?;Ok(())}).await.unwrap();
        let expected = json!({"last_successful_attempt":{"attempt_id":"newest","input_tokens":5,"cached_tokens":1,"started_at":"2020-01-01T00:00:00Z","ended_at":null,"next_request_pending":true}});
        assert_eq!(compactor.context_metrics("t").await.unwrap(), expected);
        let raw = db
            .call(|db| {
                let tx = db.transaction()?;
                let trace = crate::frontend::sql_trace::Statements::new(&tx);
                let raw = read_context_usage_tx(&tx, "t", Some(512))?;
                assert_eq!(trace.count(), 2);
                drop(trace);
                tx.commit()?;
                Ok(raw)
            })
            .await
            .unwrap();
        assert_eq!(raw.scope(), ("t", "p", "m"));
        db.shutdown().await.unwrap();
        assert_eq!(
            prepare_context_usage(raw).0,
            expected,
            "raw scalars remain sufficient after DB shutdown"
        );
    }
    #[tokio::test]
    async fn huge_and_malformed_usage_preserve_exact_scalars_unknown_values_and_saved_bytes() {
        let (_root, db, compactor) = fixture().await;
        let huge=json!({"input_tokens":50,"input_tokens_details":{"cached_tokens":12,"extra":"x".repeat(128*1024)},"attribution":"x".repeat(128*1024)}).to_string();
        attempt(
            &db,
            "huge",
            "completed",
            "p",
            "m",
            Some(huge.clone()),
            "now",
        )
        .await;
        for (body,input,cached) in [(huge,Some(50),Some(12)),("{".into(),None,None),("{}".into(),None,None),(r#"{"input_tokens":-1,"input_tokens_details":{"cached_tokens":false}}"#.into(),None,None),(r#"{"input_tokens":1.0,"input_tokens_details":{"cached_tokens":"1"}}"#.into(),None,None),(r#"{"input_tokens":9223372036854775808,"input_tokens_details":[]}"#.into(),None,None),(r#"{"input_tokens":0,"input_tokens_details":{"cached_tokens":9223372036854775807}}"#.into(),Some(0),Some(i64::MAX as u64))] {
            let saved=body.clone();db.call(move|db|{db.execute("UPDATE model_attempt SET usage_json=?1 WHERE id='huge'",[body])?;Ok(())}).await.unwrap();
            let canonical=compactor.context_metrics("t").await.unwrap();let raw=db.call(|db|read_context_usage_tx(db,"t",Some(512))).await.unwrap();
            assert_eq!(prepare_context_usage(raw).0,canonical);assert_eq!(canonical["last_successful_attempt"]["input_tokens"],json!(input));assert_eq!(canonical["last_successful_attempt"]["cached_tokens"],json!(cached));
            db.call(move|db|{assert_eq!(db.query_row("SELECT usage_json FROM model_attempt WHERE id='huge'",[],|row|row.get::<_,String>(0))?,saved);Ok(())}).await.unwrap();
        }
        // No fallback to older known numbers when the newest completed row has
        // no recorded usage; absence remains different from a numeric zero.
        attempt(&db, "unknown-latest", "completed", "p", "m", None, "later").await;
        let value = compactor.context_metrics("t").await.unwrap();
        assert_eq!(
            value["last_successful_attempt"]["attempt_id"],
            "unknown-latest"
        );
        assert!(value["last_successful_attempt"]["input_tokens"].is_null());
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn missing_deleted_and_profile_changes_keep_the_canonical_target_contract() {
        let (_root, db, compactor) = fixture().await;
        attempt(&db, "old", "completed", "p", "m", Some("{}".into()), "now").await;
        db.call(|db| {
            db.execute("UPDATE thread SET model_id='new-model' WHERE id='t'", [])?;
            Ok(())
        })
        .await
        .unwrap();
        let raw = db
            .call(|db| read_context_usage_tx(db, "t", Some(512)))
            .await
            .unwrap();
        assert_eq!(raw.scope(), ("t", "p", "new-model"));
        assert_eq!(
            prepare_context_usage(raw).0,
            json!({"last_successful_attempt":null})
        );
        assert_eq!(
            compactor.context_metrics("missing").await.unwrap_err().code,
            ErrorCode::ThreadNotFound
        );
        db.call(|db| {
            db.execute("UPDATE thread SET deleted_at='now' WHERE id='t'", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            compactor.context_metrics("t").await.unwrap_err().code,
            ErrorCode::ThreadNotFound
        );
        assert!(
            db.call(|db| read_context_usage_tx(db, "t", Some(512)))
                .await
                .is_err()
        );
        db.shutdown().await.unwrap();
    }
    #[test]
    fn invalid_display_limits_and_oversized_target_fail_before_database_access() {
        let db = Connection::open_in_memory().unwrap();
        for bytes in [0, 513, usize::MAX] {
            let result = read_context_usage_tx(&db, "t", Some(bytes));
            assert!(matches!(result, Err(StorageError::Operation(error))
                if error.code == ErrorCode::InvalidArgument));
        }
        let result = read_context_usage_tx(&db, &"x".repeat(513), Some(512));
        assert!(matches!(result, Err(StorageError::Operation(error))
            if error.code == ErrorCode::InputLimit));
    }
    #[tokio::test]
    async fn display_scalar_overflow_is_explicit_and_does_not_change_canonical_cli() {
        let (_root, db, compactor) = fixture().await;
        attempt(
            &db,
            "huge-cell",
            "completed",
            "p",
            "m",
            Some("{}".into()),
            "now",
        )
        .await;
        for column in ["started_at", "ended_at"] {
            db.call(move |db| {
                db.execute(
                    &format!("UPDATE model_attempt SET {column}=?1 WHERE id='huge-cell'"),
                    ["界".repeat(400_000)],
                )?;
                Ok(())
            })
            .await
            .unwrap();
            let error = db
                .call(|db| read_context_usage_tx(db, "t", Some(512)))
                .await;
            assert!(
                matches!(error, Err(StorageError::Operation(error)) if error.code == ErrorCode::InputLimit)
            );
            let exact = compactor.context_metrics("t").await.unwrap();
            assert_eq!(
                exact["last_successful_attempt"][column]
                    .as_str()
                    .unwrap()
                    .len(),
                1_200_000
            );
            db.call(move |db| {
                db.execute(
                    &format!("UPDATE model_attempt SET {column}=?1 WHERE id='huge-cell'"),
                    ["now"],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        }
        for column in ["profile_id", "model_id"] {
            db.call(move |db| {
                db.execute(
                    &format!("UPDATE thread SET {column}=?1 WHERE id='t'"),
                    ["x".repeat(1024 * 1024)],
                )?;
                Ok(())
            })
            .await
            .unwrap();
            let error = db
                .call(|db| read_context_usage_tx(db, "t", Some(512)))
                .await;
            assert!(
                matches!(error, Err(StorageError::Operation(error)) if error.code == ErrorCode::InputLimit)
            );
            assert_eq!(
                compactor.context_metrics("t").await.unwrap(),
                json!({"last_successful_attempt":null})
            );
            db.call(move |db| {
                db.execute(
                    &format!("UPDATE thread SET {column}=?1 WHERE id='t'"),
                    [if column == "profile_id" { "p" } else { "m" }],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        }
        let raw = db
            .call(|db| read_context_usage_tx(db, "t", Some(512)))
            .await
            .unwrap();
        let (saved, scope) = prepare_context_usage(raw);
        assert_eq!(scope, ("t".into(), "p".into(), "m".into()));
        assert_eq!(saved, compactor.context_metrics("t").await.unwrap());
        db.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod query_cost_tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    use rusqlite::StatementStatus;

    #[derive(Debug, PartialEq, Eq)]
    struct Cost {
        attempt: Option<String>,
        pending: bool,
        vm: i32,
        sorts: i32,
        fullscan: i32,
    }
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now'),('other','/','','p','m','now');
          INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'),('other-r','other','completed','now');
          INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now'),('other-u','other','other-r','completed','now');
          INSERT INTO node(id,kind,created_at,payload_json)VALUES('marker','summary','now','{}');").unwrap();
        db
    }
    fn attempts(db: &Connection, turn: &str, prefix: &str, from: usize, until: usize) {
        db.execute_batch("BEGIN").unwrap();
        for i in from..until {
            db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES(?1,?2,0,0,'p','m',0,'completed','{}','{\"input_tokens\":11,\"input_tokens_details\":{\"cached_tokens\":3}}','2026-01-01T00:00:00Z')",params![format!("{prefix}-{i:06}"),turn]).unwrap();
        }
        db.execute_batch("COMMIT").unwrap();
    }
    fn replacements(
        db: &Connection,
        thread: &str,
        prefix: &str,
        from: usize,
        until: usize,
        stamp: &str,
    ) {
        db.execute_batch("BEGIN").unwrap();
        for i in from..until {
            let id = format!("{prefix}-{i:06}");
            db.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES(?1,'marker',?1,'summary-v1','fixture','completed',?2)",params![id,stamp]).unwrap();
            db.execute(
                "INSERT INTO thread_replacement(thread_id,replacement_id)VALUES(?1,?2)",
                params![thread, id],
            )
            .unwrap();
        }
        db.execute_batch("COMMIT").unwrap();
    }
    fn measure(db: &Connection) -> Cost {
        let trace = Statements::new(db);
        let mut statement = db.prepare(&latest_query()).unwrap();
        let rows = statement
            .query_map(params!["t", 512], |row| {
                Ok((row.get::<_, Option<String>>(0)?, row.get::<_, bool>(8)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            trace.count(),
            1,
            "measure the actual shared latest query including replacement EXISTS"
        );
        Cost {
            attempt: rows[0].0.clone(),
            pending: rows[0].1,
            vm: statement.get_status(StatementStatus::VmStep),
            sorts: statement.get_status(StatementStatus::Sort),
            fullscan: statement.get_status(StatementStatus::FullscanStep),
        }
    }
    fn prepared(db: &Connection) -> Value {
        let trace = Statements::new(db);
        let raw = read_context_usage_tx(db, "t", Some(512)).unwrap();
        assert_eq!(
            trace.count(),
            2,
            "one exact scope read and one shared latest query; query count is not a work bound"
        );
        drop(trace);
        let display = prepare_context_usage(raw).0;
        let exact = prepare_context_usage(read_context_usage_tx(db, "t", None).unwrap()).0;
        if display["bounded_observation"].is_null() {
            assert_eq!(display, exact);
        } else {
            assert!(display["bounded_observation"].is_object());
        }
        display
    }
    fn plan(db: &Connection) -> Vec<String> {
        db.prepare(&format!("EXPLAIN QUERY PLAN {}", latest_query()))
            .unwrap()
            .query_map(params!["t", 512], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }
    #[test]
    fn latest_context_query_is_foreign_scoped_and_records_matching_own_attempt_growth() {
        let db = fixture();
        attempts(&db, "u", "own", 0, 1);
        let first = measure(&db);
        attempts(&db, "other-u", "foreign", 0, 5000);
        let foreign = measure(&db);
        assert_eq!(
            foreign, first,
            "unrelated matching profile/model attempts must not become a global rowid scan"
        );
        attempts(&db, "u", "own", 1, 33);
        let own33 = measure(&db);
        attempts(&db, "u", "own", 33, 1057);
        let own1057 = measure(&db);
        assert_eq!(own1057.attempt.as_deref(), Some("own-001056"));
        assert!(
            own1057.vm > own33.vm && own33.vm > first.vm,
            "record the real remaining own-lifetime cost, not a false LIMIT1 bound: {first:?}->{own33:?}->{own1057:?}"
        );
        assert!(own1057.sorts > 0);
        assert_eq!(own1057.fullscan, 0);
        let details = prepared(&db);
        assert_eq!(
            details["bounded_observation"]["candidate"]["attempt_id"],
            "own-001056"
        );
        assert_eq!(
            details["bounded_observation"]["candidate"]["input_tokens"],
            11
        );
        assert!(details["last_successful_attempt"].is_null());
        let plan = plan(&db);
        assert!(
            plan.iter().any(|p| p.contains("turns_by_thread")),
            "{plan:?}"
        );
        assert!(
            plan.iter().any(|p| p.contains("attempts_by_turn")),
            "{plan:?}"
        );
        assert!(plan.iter().any(|p| p.contains("TEMP B-TREE")), "{plan:?}");
        assert!(
            !plan
                .iter()
                .any(|p| p.starts_with("SCAN a") || p.starts_with("SCAN u")),
            "{plan:?}"
        );
        eprintln!(
            "Context latest attempt actual query counts: first={first:?}, foreign5000={foreign:?}, own33={own33:?}, own1057={own1057:?}; own work remains unbounded"
        );
    }
    #[test]
    fn replacement_pending_probe_is_foreign_scoped_and_records_negative_own_membership_growth() {
        let db = fixture();
        attempts(&db, "u", "own", 0, 1);
        replacements(&db, "t", "old", 0, 1, "2020-01-01T00:00:00Z");
        let first = measure(&db);
        replacements(&db, "other", "foreign", 0, 5000, "2099-01-01T00:00:00Z");
        let foreign = measure(&db);
        assert_eq!(
            foreign, first,
            "foreign newer replacements must neither change the pending flag nor add SQL work"
        );
        replacements(&db, "t", "old", 1, 2048, "2020-01-01T00:00:00Z");
        let own2048 = measure(&db);
        assert!(!own2048.pending);
        assert!(
            own2048.vm > first.vm,
            "a false EXISTS can scan every own membership: {first:?}->{own2048:?}"
        );
        assert_eq!(
            own2048.fullscan, 0,
            "indexed membership is still not a finite own-lifetime bound"
        );
        replacements(&db, "t", "zz-witness", 0, 1, "2099-01-01T00:00:00Z");
        let witness = measure(&db);
        assert!(witness.pending);
        let details = prepared(&db);
        assert_eq!(
            details["last_successful_attempt"]["next_request_pending"],
            true
        );
        let plan = plan(&db);
        assert!(
            plan.iter()
                .any(|p| p.contains("SEARCH tr USING COVERING INDEX") && p.contains("thread_id=?")),
            "{plan:?}"
        );
        assert!(
            plan.iter()
                .any(|p| p.contains("SEARCH r USING INDEX") && p.contains("id=?")),
            "{plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|p| p.starts_with("SCAN tr") || p.starts_with("SCAN r")),
            "{plan:?}"
        );
        eprintln!(
            "Context replacement actual query counts: old1={first:?}, foreign5000={foreign:?}, old2048={own2048:?}, positive_witness={witness:?}; negative own work remains unbounded"
        );
    }
}
#[cfg(test)]
mod bounded_cost_tests {
    use super::*;
    use rusqlite::StatementStatus;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();

        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO node(id,kind,created_at,payload_json)VALUES('marker','summary','now','{}');").unwrap();
        db
    }
    fn grow(db: &Connection, from: usize, to: usize) {
        db.execute_batch("BEGIN").unwrap();
        for i in from..to {
            let turn = format!("u{i:06}");
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,'t','r','completed',?2)",params![turn,format!("{i:08}")]).unwrap();
            for a in 0..64 {
                db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES(?1,?2,0,0,'p','m',0,'completed','{}','{\"input_tokens\":11}',?3)",params![format!("a{i:06}-{a:02}"),turn,format!("{a:08}")]).unwrap();
            }
            for edit in 0..4 {
                let id = format!("edit{i:06}-{edit}");
                db.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES(?1,'marker',?1,'summary-v1','fixture','completed','0000')",[&id]).unwrap();
                db.execute(
                    "INSERT INTO thread_replacement(thread_id,replacement_id)VALUES('t',?1)",
                    [id],
                )
                .unwrap();
            }
        }
        db.execute_batch("COMMIT").unwrap();
    }
    fn measure(db: &Connection) -> (i32, i32) {
        let mut q = db.prepare(&bounded::query()).unwrap();
        {
            let mut rows = q.query(params!["t", 512]).unwrap();
            assert!(rows.next().unwrap().is_some());
            assert!(rows.next().unwrap().is_none());
        }
        (
            q.get_status(StatementStatus::VmStep),
            q.get_status(StatementStatus::FullscanStep),
        )
    }
    #[test]
    fn tenfold_owned_growth_keeps_actual_display_query_vm_constant() {
        let db = fixture();
        grow(&db, 0, 64);
        let a = measure(&db);
        grow(&db, 64, 640);
        let b = measure(&db);
        assert!(b.0 <= a.0, "VM grew under10x owned history: {a:?}->{b:?}");
        assert!(
            a.0 < 200_000 && b.1 < 10_000,
            "unbounded native work: {a:?} {b:?}"
        );
        let v = prepare_context_usage(read_context_usage_tx(&db, "t", Some(512)).unwrap()).0;
        assert!(v["last_successful_attempt"].is_null());
        assert_eq!(v["bounded_observation"]["latest_complete"], false);
        assert!(v["bounded_observation"]["candidate"]["next_request_pending"].is_null());
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {}", bounded::query()))
            .unwrap()
            .query_map(params!["t", 512], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter().any(|s| s.contains("attempts_by_turn")),
            "{plan:?}"
        );
        assert!(
            plan.iter().any(|s| s.contains("INTEGER PRIMARY KEY")),
            "{plan:?}"
        );
        assert!(!plan.iter().any(|s| s.contains("AUTOMATIC")), "{plan:?}");
        eprintln!(
            "bounded context actual VM/fullscan before/after10x: {a:?}->{b:?}; elapsed is not a gate"
        );
    }
    #[test]
    fn rollback_rowid_winner_is_not_falsely_claimed_as_timestamp_prefix_latest() {
        let db = fixture();
        grow(&db, 0, 1);
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('late-rowid','u000000',0,0,'p','m',0,'completed','{}','00000000')",[]).unwrap();
        let exact = read_context_usage_tx(&db, "t", None).unwrap();
        assert_eq!(exact.latest.as_ref().unwrap().attempt_id, "late-rowid");
        assert!(exact.latest.as_ref().unwrap().usage.input_tokens.is_none());
        let v = prepare_context_usage(read_context_usage_tx(&db, "t", Some(512)).unwrap()).0;
        assert!(v["last_successful_attempt"].is_null());
        assert_eq!(v["bounded_observation"]["latest_complete"], false);
        assert_ne!(
            v["bounded_observation"]["candidate"]["attempt_id"],
            "late-rowid"
        );
    }
}
