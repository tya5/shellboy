//! Bounded multi-input transport with transactional adoption and durable fallback.
use super::*;

impl IndexService {
    pub(super) async fn process_claims(
        &self,
        key: &str,
        mut claims: Vec<Claimed>,
        cancel: CancellationToken,
    ) -> Result<(), ApiError> {
        if !self.capacity_allowed(key).await? {
            return self
                .fail_claims(
                    key,
                    claims,
                    ModelError::new(
                        "storage_limit",
                        "Logical data limit reached before embedding; retry after collection",
                    ),
                    false,
                )
                .await;
        }
        let first = claims
            .first()
            .ok_or_else(|| err(ErrorCode::InvalidState, "Empty embedding claim"))?;
        let requested_dimensions = first.requested_dimensions;
        let connection_name = first
            .space
            .as_ref()
            .map(|space| space.profile.as_str())
            .or(self.config.embedding.connection.as_deref())
            .ok_or_else(|| err(ErrorCode::ConfigInvalid, "Embedding connection missing"))?
            .to_owned();
        let model = first
            .space
            .as_ref()
            .map(|space| space.model.as_str())
            .or(self.config.embedding.model.as_deref())
            .ok_or_else(|| err(ErrorCode::ConfigInvalid, "Embedding model missing"))?
            .to_owned();
        let client = match self.clients.get(&connection_name) {
            Ok(client) => client,
            Err(error) => return self.handle_request_failure(key, claims, error).await,
        };
        let lease = match client.acquire_rate(&cancel).await {
            Ok(lease) => lease,
            Err(error) => return self.defer_before_send(claims, &error.kind).await,
        };
        if cancel.is_cancelled() {
            return self.defer_before_send(claims, "cancelled").await;
        }
        let checked_key = key.to_owned();
        let mut pause_keys = self.identity_keys();
        pause_keys.push(checked_key.clone());
        if let Some(family) = self.failure_key() {
            pause_keys.push(family.to_owned());
        }
        let (retained, changed) = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let mut kept = Vec::new();
                let mut changed = false;
                let mut paused = false;
                for key in &pause_keys {
                    paused |= meta(&tx, &IndexKeys::new(key).pause())?.is_some();
                }
                for claimed in claims {
                    if !same_claim(&tx, &claimed)? {
                        continue;
                    }
                    if paused {
                        changed |= defer_before_send_tx(&tx, &claimed, "index_paused")?;
                    } else if !witness_valid(&tx, &claimed.work.witness, &claimed.work.body_kind)?
                        || !group_current(&tx, IndexKeys::new(&checked_key), &claimed.work.group)?
                    {
                        if let Some(job) = &claimed.job_id {
                            tx.execute("UPDATE embedding_job SET attempts=MAX(attempts-1,0) WHERE id=?1 AND state='running'", [job])?;
                        }
                        changed |= discard_claim(
                            &tx,
                            &claimed,
                            "Source or index group changed during admission",
                        )?;
                    } else {
                        let keys = IndexKeys::new(&checked_key);
                        let selected = space_for_group(&tx, keys, &claimed.work.group)?;
                        let unchanged = match (&claimed.space, selected) {
                            (None, None) => true,
                            (Some(expected), Some(current)) => {
                                current.id == expected.id
                                    && matches!(current.state.as_str(), "active" | "candidate")
                                    && current.profile == expected.profile
                                    && current.model == expected.model
                                    && current.dimension == expected.dimension
                                    && current.requested_dimensions == expected.requested_dimensions
                                    && current.splitter == expected.splitter
                                    && current.table == expected.table
                            }
                            _ => false,
                        };
                        if !unchanged || dimensions::candidate_stopped(&tx, keys, &claimed.work.group)? {
                            changed |= defer_before_send_tx(&tx, &claimed, "space_changed_during_admission")?;
                        } else {
                            kept.push(claimed);
                        }
                    }
                }
                tx.commit()?;
                Ok((kept, changed))
            })
            .await
            .map_err(db_error)?;
        self.publish_saved(changed);
        claims = retained;
        let Some(first) = claims.first() else {
            return Ok(());
        };
        if !self.capacity_allowed(key).await? {
            return self.defer_before_send(claims, "storage_limit").await;
        }
        if cancel.is_cancelled() {
            return self.defer_before_send(claims, "cancelled").await;
        }
        let input = claims
            .iter()
            .map(|claim| claim.work.segment.text.clone())
            .collect::<Vec<_>>();
        let result = client
            .embeddings_admitted(
                &model,
                &input,
                first.space.as_ref().map(|space| space.dimension),
                requested_dimensions,
                cancel.clone(),
                lease,
            )
            .await;
        if cancel.is_cancelled() {
            return self
                .fail_claims(
                    key,
                    claims,
                    ModelError::new("cancelled", "Embedding cancelled; retry explicitly"),
                    true,
                )
                .await;
        }
        let response = match result {
            Ok(response) => response,
            Err(error) => return self.handle_request_failure(key, claims, error).await,
        };
        if response.dimension > crate::storage::MAX_VECTOR_DIMENSIONS {
            return self.handle_request_failure(key,claims,ModelError::new("dimension_mismatch","sqlite-vec v0.1.9 supports at most8192 dimensions; choose a supported embedding profile")).await;
        }
        // The HTTP parser checks the complete permutation/count/finite shape;
        // retain a boundary check before any zip can silently omit a claim.
        if response.vectors.len() != claims.len() {
            return self
                .handle_request_failure(
                    key,
                    claims,
                    ModelError::new("protocol", "Embedding count differs from claimed inputs"),
                )
                .await;
        }
        let recovery = claims.clone();
        let failure_key = self.failure_key().unwrap_or(key).to_owned();
        let adoptions = claims
            .into_iter()
            .zip(response.vectors)
            .map(|(claimed, vector)| {
                let mut embedding =
                    identity::EmbeddingIdentity::configured(&self.config, key.to_owned());
                embedding.requested_dimensions = requested_dimensions;
                store::EmbeddingAdoption {
                    failure_key: failure_key.clone(),
                    claimed,
                    embedding,
                    dimension: response.dimension,
                    vector,
                }
            })
            .collect::<Vec<_>>();
        let adopted = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let mut changed = false;
                for adoption in adoptions {
                    changed |= adoption.adopt_embedding_tx(&tx)?;
                }
                tx.commit()?;
                Ok(changed)
            })
            .await;
        if let Ok(changed) = &adopted {
            self.publish_saved(*changed);
            if *changed {
                self.spaces_changed(); // adoption may activate a rebuild candidate
            }
        }
        if let Err(error) = adopted {
            let error = error.into_api_error();
            let kind = serde_json::to_value(error.code)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "invalid_state".into());
            // The vector transaction rolled back as a whole. Preserve each
            // running input as failed; do not replay already-billed HTTP.
            self.fail_claims(key, recovery, ModelError::new(&kind, error.message), false)
                .await?;
        }
        Ok(())
    }

    async fn defer_before_send(&self, claims: Vec<Claimed>, kind: &str) -> Result<(), ApiError> {
        let kind = kind.to_owned();
        let changed = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let mut changed = false;
                for claim in claims {
                    changed |= defer_before_send_tx(&tx, &claim, &kind)?;
                }
                tx.commit()?;
                Ok(changed)
            })
            .await
            .map_err(db_error)?;
        self.publish_saved(changed);
        Ok(())
    }

    async fn handle_request_failure(
        &self,
        key: &str,
        claims: Vec<Claimed>,
        error: ModelError,
    ) -> Result<(), ApiError> {
        if claims.len() == 1 {
            if self
                .fail_initial_dimensions(key, &claims[0], &error)
                .await?
            {
                return Ok(());
            }
            return self.fail_claims(key, claims, error, false).await;
        }
        let failure = error.embedding_failure();
        // A malformed multi-result dimension is attributable only after
        // singleton fallback. Credentials/missing configuration remain global.
        let immediate = failure.credentials_or_credit
            || (failure.invalid_configuration && error.kind != "dimension_mismatch");
        if immediate {
            return self.fail_claims(key, claims, error, false).await;
        }
        let key = key.to_owned();
        let changed = self.database.call(move |db| {
            let tx = db.transaction()?;
            let detail = json!({"kind":"embedding_batch_failure","input_count":claims.len(),"error":error,"fallback":"single"});
            let mut changed = false;
            for claimed in claims {
                if !claim_valid(&tx,&claimed)? || !witness_valid(&tx,&claimed.work.witness,&claimed.work.body_kind)? || !group_current(&tx,IndexKeys::new(&key),&claimed.work.group)? {
                    changed |= discard_claim(&tx,&claimed,"Source or group changed before batch failure adoption")?;
                    continue;
                }
                let Some(job) = claimed.job_id else {
                    return Err(StorageError::from(err(ErrorCode::InvalidState,"Unknown-space input entered a batch")));
                };
                let mut work = claimed.work;
                work.state = "pending".into();
                work.next_at = None;
                work.error = Some(json!({"batch_fallback":detail}));
                tx.execute("UPDATE embedding_job SET state='pending',error_json=?2,next_attempt_at=NULL WHERE id=?1",params![job,detail.to_string()])?;
                changed = true; // validated running claim was returned to pending
                if update_json_meta(&tx,&IndexKeys::input(&job),&work)? != 1 {
                    return Err(serde_error());
                }
            }
            tx.commit()?;
            Ok(changed)
        }).await.map_err(db_error)?;
        self.publish_saved(changed);
        Ok(())
    }

    async fn fail_claims(
        &self,
        key: &str,
        claims: Vec<Claimed>,
        error: ModelError,
        cancelled: bool,
    ) -> Result<(), ApiError> {
        for claimed in claims {
            self.fail_work(key, claimed, error.clone(), cancelled)
                .await?;
        }
        Ok(())
    }
}

fn defer_before_send_tx(
    tx: &Transaction<'_>,
    claimed: &Claimed,
    kind: &str,
) -> Result<bool, StorageError> {
    if !same_claim(tx, claimed)? {
        return Ok(false);
    }
    let error = json!({"kind":kind,"message":"Embedding admission deferred before any provider request; retry explicitly"});
    let mut work = claimed.work.clone();
    work.attempts = work.attempts.saturating_sub(1);
    work.state = "failed".into();
    work.next_at = None;
    work.error = Some(error.clone());
    let mut changed = false;
    if let Some(job) = &claimed.job_id {
        changed |= tx.execute("UPDATE embedding_job SET state='failed',attempts=MAX(attempts-1,0),error_json=?2,next_attempt_at=NULL WHERE id=?1 AND state='running'", params![job,error.to_string()])? > 0;
        if update_json_meta(tx, &IndexKeys::input(job), &work)? != 1 {
            return Err(serde_error());
        }
    }
    if let Some(key) = &claimed.bootstrap_key {
        changed |= update_json_meta(tx, key, &work)? > 0;
    }
    Ok(changed)
}

fn same_claim(db: &Connection, claimed: &Claimed) -> Result<bool, StorageError> {
    if !claim_valid(db, claimed)? {
        return Ok(false);
    }
    if let Some(job) = &claimed.job_id {
        return Ok(db.query_row(
            "SELECT attempts FROM embedding_job WHERE id=?1",
            [job],
            |r| r.get::<_, i64>(0),
        )? == claimed.work.attempts as i64);
    }
    if let Some(key) = &claimed.bootstrap_key {
        return Ok(
            json_meta::<Work>(db, key)?.is_some_and(|work| work.attempts == claimed.work.attempts)
        );
    }
    Ok(false)
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    fn work() -> Work {
        let mut work = Work::pending(
            Witness {
                node_id: "node".into(),
                revision: 1,
                body_hash: None,
                raw: None,
            },
            "user".into(),
            Segment {
                start: 0,
                end: 1,
                text: "x".into(),
            },
            "active",
            "explicit",
        );
        work.state = "running".into();
        work.attempts = 3;
        work
    }

    #[test]
    fn deferred_bootstrap_admission_releases_attempt_without_resurrecting_a_new_claim() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        let original = work();
        db.execute(
            "INSERT INTO app_meta VALUES('boot',?1)",
            [encode_json(&original).unwrap()],
        )
        .unwrap();
        let claimed = Claimed {
            requested_dimensions: None,
            job_id: None,
            bootstrap_key: Some("boot".into()),
            space: None,
            work: original,
        };
        {
            let tx = db.transaction().unwrap();
            assert!(defer_before_send_tx(&tx, &claimed, "rate_limit_deferred").unwrap());
            tx.commit().unwrap();
        }
        let saved: Work = json_meta(&db, "boot").unwrap().unwrap();
        assert_eq!((saved.state.as_str(), saved.attempts), ("failed", 2));
        let mut newer = work();
        newer.attempts = 4;
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='boot'",
            [encode_json(&newer).unwrap()],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        assert!(!defer_before_send_tx(&tx, &claimed, "cancelled").unwrap());
        tx.commit().unwrap();
        assert_eq!(json_meta::<Work>(&db, "boot").unwrap().unwrap().attempts, 4);
    }

    #[test]
    fn deferred_admission_is_atomic_does_not_count_a_send_and_does_not_mutate_newer_claims() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE embedding_job(id TEXT PRIMARY KEY,state TEXT,attempts INTEGER,error_json TEXT,next_attempt_at TEXT);").unwrap();
        db.execute(
            "INSERT INTO embedding_job VALUES('job','running',3,NULL,NULL)",
            [],
        )
        .unwrap();
        let original = work();
        db.execute(
            "INSERT INTO app_meta VALUES(?1,?2)",
            params![IndexKeys::input("job"), encode_json(&original).unwrap()],
        )
        .unwrap();
        let claimed = Claimed {
            requested_dimensions: None,
            job_id: Some("job".into()),
            bootstrap_key: None,
            space: None,
            work: original.clone(),
        };
        {
            let tx = db.transaction().unwrap();
            assert!(defer_before_send_tx(&tx, &claimed, "rate_limit_deferred").unwrap());
        }
        assert_eq!(
            db.query_row(
                "SELECT attempts FROM embedding_job WHERE id='job'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            3
        );
        assert_eq!(
            json_meta::<Work>(&db, &IndexKeys::input("job"))
                .unwrap()
                .unwrap()
                .attempts,
            3
        );
        {
            let tx = db.transaction().unwrap();
            assert!(defer_before_send_tx(&tx, &claimed, "rate_limit_deferred").unwrap());
            tx.commit().unwrap();
        }
        let (state, attempts): (String, i64) = db
            .query_row(
                "SELECT state,attempts FROM embedding_job WHERE id='job'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((state.as_str(), attempts), ("failed", 2));
        let saved: Work = json_meta(&db, &IndexKeys::input("job")).unwrap().unwrap();
        assert_eq!((saved.state.as_str(), saved.attempts), ("failed", 2));
        db.execute(
            "UPDATE embedding_job SET state='running',attempts=4 WHERE id='job'",
            [],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        assert!(!defer_before_send_tx(&tx, &claimed, "cancelled").unwrap());
        tx.commit().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT attempts FROM embedding_job WHERE id='job'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            4
        );
    }
}
