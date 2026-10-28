//! Embedding failure adoption and bounded automatic retry.
use super::*;

// One bounded current-format record per endpoint family. Reset deletes the
// record, and malformed records never become an implicit successful reset.
#[derive(Serialize, Deserialize)]
struct FailureStreak {
    cause: String,
    count: usize,
}

fn failure_cause(error: &ModelError, transient: bool) -> String {
    if transient {
        return "transient".into();
    }
    // Without a provider code, kind/status identify a coarse failure class.
    // Per-request prose (IDs, echoed input, timings) must not defeat the breaker.
    // This groups unknown failures, not inferred identical root causes.
    let identity = json!([error.kind, error.status, error.provider_code]);
    format!("unknown:{}", hash(identity.to_string().as_bytes()))
}

fn advance_streak(
    tx: &Transaction<'_>,
    keys: IndexKeys<'_>,
    cause: String,
    threshold: usize,
) -> Result<bool, StorageError> {
    let record = keys.common_failures();
    let previous = meta(tx, &record)?
        .map(|value| decode_json::<FailureStreak>(&value))
        .transpose()?;
    let count = previous
        .filter(|previous| previous.cause == cause)
        .map_or(1, |previous| previous.count.saturating_add(1))
        .min(threshold);
    set_json_meta(tx, &record, &FailureStreak { cause, count })?;
    Ok(count >= threshold)
}

impl IndexService {
    pub(super) async fn fail_work(
        &self,
        key: &str,
        claimed: Claimed,
        error: ModelError,
        cancelled: bool,
    ) -> Result<(), ApiError> {
        let failure_key = self.failure_key().unwrap_or(key).to_owned();
        let pause_keys = self.identity_keys();
        let key = key.to_owned();
        let config = self.config.clone();
        let recovery_claim = claimed.clone();
        let provider_error = error.clone();
        let outcome = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                adopt_failure_tx(
                    &tx,
                    &claimed,
                    IndexKeys::new(&key),
                    (&failure_key, &pause_keys),
                    &error,
                    cancelled,
                    &config,
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(db_error);
        if let Err(adoption_error) = &outcome {
            let claimed = recovery_claim;
            let diagnostic = json!({
                "kind":"index_failure_adoption",
                "message":"Embedding failed but its normal failure adoption failed; retry explicitly after checking storage",
                "provider_error":provider_error,
                "adoption_error":adoption_error,
            });
            let mut pause_keys = self.identity_keys();
            if let Some(family) = self.failure_key() {
                pause_keys.push(family.to_owned());
            }
            // Keep the fixed input and actual provider failure. A separate
            // transaction must not rerun the malformed streak update.
            let _ = self.database.call(move |db| {
                let tx = db.transaction()?;
                if claim_valid(&tx, &claimed)? {
                    if let Some(job) = &claimed.job_id {
                        tx.execute("UPDATE embedding_job SET state='failed',error_json=?2,next_attempt_at=NULL WHERE id=?1",params![job,diagnostic.to_string()])?;
                    }
                    if let Some(bootstrap) = &claimed.bootstrap_key {
                        let mut work = claimed.work.clone();
                        work.state = "failed".into();
                        work.error = Some(diagnostic.clone());
                        work.next_at = None;
                        update_json_meta(&tx, bootstrap, &work)?;
                    }
                    for key in pause_keys {
                        set_meta(&tx, &IndexKeys::new(&key).pause(), &diagnostic.to_string())?;
                    }
                }
                tx.commit()?;
                Ok(())
            }).await;
        }
        outcome
    }
}

/// Adopt failure/retry/split facts only inside the caller's existing transaction.
fn adopt_failure_tx(
    tx: &Transaction<'_>,
    claimed: &Claimed,
    keys: IndexKeys<'_>,
    family: (&str, &[String]),
    error: &ModelError,
    cancelled: bool,
    config: &Config,
) -> Result<(), StorageError> {
    let (failure_key, pause_keys) = family;
    let retry_delays = &config.indexing.retry_delays_ms;
    let retries = config.indexing.retries;
    let pause_count = config.indexing.common_failure_pause_count;
    let error_json = serde_json::to_value(error).map_err(|_| {
        StorageError::from(err(
            ErrorCode::InvalidState,
            "Cannot encode embedding error",
        ))
    })?;
    let failure = error.embedding_failure();
    let immediate = failure.credentials_or_credit || failure.invalid_configuration;
    let length = !immediate && failure.input_limit;
    let common = failure.transient;
    let cause = failure_cause(error, common);
    if !claim_valid(tx, claimed)?
        || !witness_valid(tx, &claimed.work.witness, &claimed.work.body_kind)?
    {
        discard_claim(tx, claimed, "Source disappeared before error adoption")?;
        return Ok(());
    }
    if length && claimed.work.segment.end - claimed.work.segment.start > 1 && !cancelled {
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::new(failure_key).common_failures()],
        )?;
        let size = (claimed.work.segment.end - claimed.work.segment.start)
            .div_ceil(2)
            .max(1);
        let children = split_text(&claimed.work.segment.text, size, claimed.work.segment.start);
        for segment in children {
            let mut child = claimed.work.clone();
            child.segment = segment;
            child.attempts = 0;
            child.state = "pending".into();
            child.next_at = None;
            child.error = None;
            if let Some(space) = &claimed.space {
                queue_job(tx, space, &child)?;
            } else {
                tx.execute(
                    "INSERT OR IGNORE INTO app_meta(key,value)VALUES(?1,?2)",
                    params![bootstrap_key(keys, &child), encode_json(&child)?],
                )?;
            }
        }
        if let Some(job) = &claimed.job_id {
            tx.execute(
                "UPDATE embedding_job SET state='split',error_json=?2 WHERE id=?1",
                params![job, error_json.to_string()],
            )?;
            tx.execute("DELETE FROM app_meta WHERE key=?1", [IndexKeys::input(job)])?;
        }
        if let Some(bootstrap) = &claimed.bootstrap_key {
            tx.execute("DELETE FROM app_meta WHERE key=?1", [bootstrap])?;
        }
    } else {
        let retry = common && !immediate && !cancelled && claimed.work.attempts <= retries;
        let delay = if retry {
            let base = retry_delays[claimed.work.attempts.saturating_sub(1)];
            let bytes = uuid::Uuid::new_v4();
            let jitter = 0.8
                + (u16::from_le_bytes([bytes.as_bytes()[0], bytes.as_bytes()[1]]) as f64
                    / u16::MAX as f64)
                    * 0.4;
            Some((base as f64 * jitter).round() as i64)
        } else {
            None
        };
        let next = delay.map(|millis| {
            (chrono::Utc::now() + chrono::Duration::milliseconds(millis)).to_rfc3339()
        });
        let state = if retry { "pending" } else { "failed" };
        if let Some(job) = &claimed.job_id {
            tx.execute(
                "UPDATE embedding_job SET state=?2,error_json=?3,next_attempt_at=?4 WHERE id=?1",
                params![job, state, error_json.to_string(), next],
            )?;
        }
        if let Some(bootstrap) = &claimed.bootstrap_key {
            let mut work = claimed.work.clone();
            work.state = state.into();
            work.error = Some(error_json.clone());
            work.next_at = next.clone();
            update_json_meta(tx, bootstrap, &work)?;
        }
        let pause = if cancelled {
            tx.execute(
                "DELETE FROM app_meta WHERE key=?1",
                [IndexKeys::new(failure_key).common_failures()],
            )?;
            false
        } else if immediate {
            tx.execute(
                "DELETE FROM app_meta WHERE key=?1",
                [IndexKeys::new(failure_key).common_failures()],
            )?;
            true
        } else if retry {
            false
        } else {
            advance_streak(tx, IndexKeys::new(failure_key), cause, pause_count)?
        };
        if pause {
            for paused in pause_keys {
                set_meta(tx, &IndexKeys::new(paused).pause(), &error_json.to_string())?;
            }
            if let Some(space) = &claimed.space {
                tx.execute("UPDATE index_space SET state=CASE WHEN state='candidate' THEN 'candidate_paused' ELSE 'paused' END WHERE id=?1 AND state IN ('active','candidate')",[&space.id])?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cause_identity_uses_codes_or_status_classes_and_keeps_the_counter_bounded() {
        let mut a = ModelError::provider(
            Some(400),
            &json!({"error":{"code":"unknown_a","message":"first"}}),
        );
        let mut b = a.clone();
        b.message = "request-specific detail".into();
        assert_eq!(failure_cause(&a, false), failure_cause(&b, false));
        a.provider_code = None;
        b.provider_code = None;
        assert_eq!(failure_cause(&a, false), failure_cause(&b, false));
        assert_eq!(failure_cause(&a, true), failure_cause(&b, true));
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        let tx = db.transaction().unwrap();
        assert!(
            !advance_streak(&tx, IndexKeys::new("key"), "transient".into(), 3).unwrap(),
            "a missing counter starts at one"
        );
        assert!(!advance_streak(&tx, IndexKeys::new("key"), "transient".into(), 3).unwrap());
        assert!(
            !advance_streak(&tx, IndexKeys::new("key"), failure_cause(&a, false), 3).unwrap(),
            "different cause begins at one"
        );
        let value = serde_json::to_string(&FailureStreak {
            cause: failure_cause(&a, false),
            count: usize::MAX,
        })
        .unwrap();
        set_meta(&tx, "index.common_failures.key", &value).unwrap();
        assert!(advance_streak(&tx, IndexKeys::new("key"), failure_cause(&a, false), 3).unwrap());
        let value = meta(&tx, "index.common_failures.key").unwrap().unwrap();
        assert!(value.len() <= 128);
        assert_eq!(
            serde_json::from_str::<FailureStreak>(&value).unwrap().count,
            3
        );
    }
}
