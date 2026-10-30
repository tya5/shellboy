//! Explicit request modes and one-input candidate validation on existing metadata.
use super::*;
use crate::protocol::{IndexProbeArgs, IndexProbeObservation};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct CandidateMode {
    pub(super) group: String,
    pub(super) dimensions: usize,
    pub(super) error: Option<Value>,
    pub(super) paused_at: Option<String>,
}
pub(super) fn candidate_key(keys: IndexKeys<'_>) -> String {
    format!("index.dimension_candidate.{}", keys.splitter())
}
pub(super) fn space_key(space: &str) -> String {
    format!("index.space_dimensions.{space}")
}
pub(super) fn space_request(db: &Connection, space: &str) -> Result<Option<usize>, StorageError> {
    let dimension: Option<usize> = json_meta(db, &space_key(space))?;
    if dimension.is_some_and(|n| !(1..=crate::storage::MAX_VECTOR_DIMENSIONS).contains(&n)) {
        return Err(serde_error());
    }
    Ok(dimension)
}
pub(super) fn candidate_stopped(
    db: &Connection,
    keys: IndexKeys<'_>,
    group: &str,
) -> Result<bool, StorageError> {
    Ok(json_meta::<CandidateMode>(db, &candidate_key(keys))?
        .is_some_and(|mode| mode.group == group && mode.error.is_some()))
}
pub(super) fn clear_failure(tx: &Transaction<'_>, keys: IndexKeys<'_>) -> Result<(), StorageError> {
    let record = candidate_key(keys);
    if let Some(mut mode) = json_meta::<CandidateMode>(tx, &record)? {
        mode.error = None;
        mode.paused_at = None;
        set_json_meta(tx, &record, &mode)?;
    }
    Ok(())
}
pub(super) fn bind_space(
    tx: &Transaction<'_>,
    space: &str,
    requested: Option<usize>,
) -> Result<(), StorageError> {
    match (space_request(tx, space)?, requested) {
        (Some(a), Some(b)) if a != b => Err(serde_error()),
        (None, Some(n)) => set_json_meta(tx, &space_key(space), &n),
        _ => Ok(()),
    }
}
impl IndexService {
    pub(super) async fn fail_initial_dimensions(
        &self,
        key: &str,
        claimed: &Claimed,
        error: &ModelError,
    ) -> Result<bool, ApiError> {
        if claimed.space.is_some()
            || claimed.work.group == "active"
            || error.embedding_failure().transient
            || error.embedding_failure().input_limit
            || error.embedding_failure().credentials_or_credit
            || (error.embedding_failure().invalid_configuration
                && error.kind != "dimension_mismatch")
            || error.kind == "cancelled"
        {
            return Ok(false);
        }
        let record = candidate_key(IndexKeys::new(key));
        let profile = key.to_owned();
        let group = claimed.work.group.clone();
        let claimed = claimed.clone();
        let error = serde_json::to_value(error).map_err(|_| {
            err(
                ErrorCode::InvalidState,
                "Cannot encode dimension validation failure",
            )
        })?;
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                if !claim_valid(&tx, &claimed)?
                    || !witness_valid(&tx, &claimed.work.witness, &claimed.work.body_kind)?
                    || !group_current(&tx, IndexKeys::new(&profile), &group)?
                {
                    discard_claim(
                        &tx,
                        &claimed,
                        "Source or group changed before dimension validation",
                    )?;
                    tx.commit()?;
                    return Ok(true);
                }
                let Some(mut mode) =
                    json_meta::<CandidateMode>(&tx, &record)?.filter(|mode| mode.group == group)
                else {
                    return Ok(false);
                };
                mode.error = Some(error.clone());
                mode.paused_at = Some(now());
                set_json_meta(&tx, &record, &mode)?;
                let mut q = tx.prepare("SELECT key,value FROM app_meta WHERE key LIKE ?1")?;
                let rows = q
                    .query_map([IndexKeys::new(&profile).bootstrap_pattern()], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                drop(q);
                for (key, value) in rows {
                    let mut work: Work = decode_json(&value)?;
                    if work.group == group && matches!(work.state.as_str(), "pending" | "running") {
                        work.state = "failed".into();
                        work.next_at = None;
                        work.error = Some(error.clone());
                        update_json_meta(&tx, &key, &work)?;
                    }
                }
                tx.commit()?;
                Ok(true)
            })
            .await
            .map_err(db_error)
    }
    pub async fn probe(&self, args: IndexProbeArgs) -> Result<IndexProbeObservation, ApiError> {
        crate::protocol::Command::IndexProbe(args.clone()).validate()?;
        self.key()?;
        let connection = self.config.embedding.connection.clone().unwrap();
        let model = self.config.embedding.model.clone().unwrap();
        if connection.len() > 4096 || model.len() > 4096 {
            return Err(err(
                ErrorCode::ConfigInvalid,
                "Embedding profile/model name exceeds probe record limit",
            ));
        }
        let client = self
            .clients
            .get(&connection)
            .map_err(|e| err(ErrorCode::IoFailed, e.to_string()))?;
        let result = client
            .embeddings(
                &model,
                &["Shellboy embedding connection probe.".into()],
                None,
                args.dimensions,
                self.stop.child_token(),
            )
            .await;
        let failure = result.as_ref().err().map(|e| json!({"kind":e.kind,"status":e.status,"message":e.message.chars().take(4096).collect::<String>()})).or_else(|| {
            result.as_ref().ok().filter(|response| response.dimension > crate::storage::MAX_VECTOR_DIMENSIONS).map(|response| json!({"kind":"storage_dimension_unsupported","message":format!("Observed dimension {} exceeds storage limit {}",response.dimension,crate::storage::MAX_VECTOR_DIMENSIONS)}))
        });
        let observation = IndexProbeObservation {
            connection,
            model,
            requested_dimensions: args.dimensions,
            dimension: result.as_ref().ok().map(|r| r.dimension),
            checked_at: now(),
            success: result.is_ok() && failure.is_none(),
            error: failure,
        };
        let saved = observation.clone();
        let family = self.failure_key().unwrap_or_default();
        let key = format!(
            "index.probe.{}",
            hash(format!("{family}\0{:?}", args.dimensions).as_bytes())
        );
        self.database.call(move |db| {
            let tx=db.transaction()?; set_json_meta(&tx,&key,&saved)?;
            tx.execute("DELETE FROM app_meta WHERE key IN (SELECT key FROM app_meta WHERE key LIKE 'index.probe.%' ORDER BY json_extract(value,'$.checked_at') DESC,key DESC LIMIT -1 OFFSET 32)",[])?;
            tx.commit()?; Ok(())
        }).await.map_err(db_error)?;
        if !observation.success {
            let mut error = err(
                ErrorCode::IoFailed,
                "Embedding probe failed; inspect the recorded result; no alternative dimensions were tried",
            );
            error.details = Some(json!({"probe":observation}));
            return Err(error);
        }
        Ok(observation)
    }
}
