//! Saved context usage shared by canonical context metrics and display reads.
//! Caller owns the connection; JSON preparation follows the read transaction.
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
    usage::{self, StandardUsage},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(crate) type Scope = (String, String, String);
pub(crate) struct RawContextUsage {
    scope: Scope,
    latest: Option<RawSuccess>,
}
struct RawSuccess {
    attempt_id: String,
    usage: StandardUsage,
    started_at: String,
    ended_at: Option<String>,
    next_request_pending: bool,
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
/// None preserves the canonical CLI scalar contract. Display callers pass512.
/// Copy bounds are not a claim of constant latest-attempt/replacement SQL work.
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
    // The exact saved rowid ordering and replacement timestamp semantics are
    // deliberately shared with the CLI. A newer unknown usage never falls back.
    let sql = format!("SELECT
        CASE WHEN octet_length(a.id)<=?2 THEN a.id END, {},
        CASE WHEN octet_length(a.started_at)<=?2 THEN a.started_at END,
        CASE WHEN octet_length(a.ended_at)<=?2 THEN a.ended_at END,
        EXISTS(SELECT 1 FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id
          WHERE tr.thread_id=t.id AND r.created_at>a.started_at),
        COALESCE(octet_length(a.id)>?2 OR octet_length(a.started_at)>?2
          OR COALESCE(octet_length(a.ended_at)>?2,0),0)
        FROM thread t JOIN turn u ON u.thread_id=t.id JOIN model_attempt a ON a.turn_id=u.id
        WHERE t.id=?1 AND a.state='completed' AND a.profile_id=t.profile_id AND a.model_id=t.model_id
        ORDER BY a.rowid DESC LIMIT 1", usage::sql_projection("a.usage_json"));
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
