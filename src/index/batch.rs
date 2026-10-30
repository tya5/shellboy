//! Bounded multi-input transport with transactional adoption and durable fallback.
use super::*;

impl IndexService {
    pub(super) async fn process_claims(
        &self,
        key: &str,
        claims: Vec<Claimed>,
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
            .ok_or_else(|| err(ErrorCode::ConfigInvalid, "Embedding connection missing"))?;
        let model = first
            .space
            .as_ref()
            .map(|space| space.model.as_str())
            .or(self.config.embedding.model.as_deref())
            .ok_or_else(|| err(ErrorCode::ConfigInvalid, "Embedding model missing"))?;
        let input = claims
            .iter()
            .map(|claim| claim.work.segment.text.clone())
            .collect::<Vec<_>>();
        let result = if self.config.connections.contains_key(connection_name) {
            match self.clients.get(connection_name) {
                Ok(client) => {
                    client
                        .embeddings(
                            model,
                            &input,
                            first.space.as_ref().map(|space| space.dimension),
                            requested_dimensions,
                            cancel.clone(),
                        )
                        .await
                }
                Err(error) => Err(error),
            }
        } else {
            Err(ModelError::new(
                "config_missing",
                "Embedding connection no longer exists",
            ))
        };
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
                for adoption in adoptions {
                    adoption.adopt_embedding_tx(&tx)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await;
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
        self.database.call(move |db| {
            let tx = db.transaction()?;
            let detail = json!({"kind":"embedding_batch_failure","input_count":claims.len(),"error":error,"fallback":"single"});
            for claimed in claims {
                if !claim_valid(&tx,&claimed)? || !witness_valid(&tx,&claimed.work.witness,&claimed.work.body_kind)? || !group_current(&tx,IndexKeys::new(&key),&claimed.work.group)? {
                    discard_claim(&tx,&claimed,"Source or group changed before batch failure adoption")?;
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
                if update_json_meta(&tx,&IndexKeys::input(&job),&work)? != 1 {
                    return Err(serde_error());
                }
            }
            tx.commit()?;
            Ok(())
        }).await.map_err(db_error)
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
