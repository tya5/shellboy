//! Bounded read-only space, probe and historical pause projections.
use super::*;
use crate::protocol::{IndexProbeObservation, IndexSpaceStatus};
pub(super) fn enrich(
    db: &Connection,
    status: &mut IndexStatus,
    keys: &[String],
    family: Option<&str>,
) -> Result<(), StorageError> {
    let mut pause_keys = family
        .into_iter()
        .map(|key| IndexKeys::new(key).pause())
        .collect::<Vec<_>>();
    pause_keys.extend(keys.iter().map(|key| IndexKeys::new(key).pause()));
    for key in pause_keys {
        if let Some(value) = meta(db, &key)? {
            let (at, id) = pause::facts(&key, &value);
            status.paused_at = at;
            status.pause_id = Some(id);
            break;
        }
    }
    status.spaces_total = db.query_row("SELECT count(*) FROM index_space", [], |r| {
        r.get::<_, i64>(0)
    })? as u64;
    let mut q =
        db.prepare("SELECT id FROM index_space ORDER BY (id=?1) DESC,rowid DESC LIMIT 200")?;
    let ids = q
        .query_map([&status.space_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(q);
    status.spaces_truncated = status.spaces_total > ids.len() as u64;
    for id in ids {
        let space = load_space(db, &id)?.ok_or_else(serde_error)?;
        let count = db.query_row(
            "SELECT count(*) FROM embedding_chunk WHERE space_id=?1",
            [&id],
            |r| r.get::<_, i64>(0),
        )? as u64;
        let bytes = count
            .checked_mul(space.dimension as u64)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(serde_error)?;
        status.spaces.push(IndexSpaceStatus {
            space_id: id.clone(),
            connection: space.profile,
            model: space.model,
            dimension: space.dimension,
            requested_dimensions: space.requested_dimensions,
            segments: count,
            vector_payload_bytes: bytes,
            state: space.state,
            selected: status.space_id.as_deref() == Some(&id),
        });
    }
    let mut q=db.prepare("SELECT value FROM app_meta WHERE key LIKE 'index.probe.%' ORDER BY json_extract(value,'$.checked_at') DESC,key DESC LIMIT 32")?;
    for value in q.query_map([], |r| r.get::<_, String>(0))? {
        status
            .probes
            .push(decode_json::<IndexProbeObservation>(&value?)?);
    }
    for key in keys {
        if let Some(mode) = json_meta::<dimensions::CandidateMode>(
            db,
            &dimensions::candidate_key(IndexKeys::new(key)),
        )? && let Some(error) = mode.error
        {
            let saved = status.reason.take();
            status.reason = Some(format!(
                "Candidate rebuild stopped at requested dimension {} ({}): {}; {}{}",
                mode.dimensions,
                mode.paused_at.as_deref().unwrap_or("time unknown"),
                error,
                if status.space_id.is_some() {
                    "existing active index is unchanged"
                } else {
                    "no active index is selected"
                },
                saved.map(|s| format!("; {s}")).unwrap_or_default()
            ));
            break;
        }
    }
    Ok(())
}

impl IndexService {
    /// Preview only already-authorized failed work; never authorize old history.
    pub(super) async fn retry_preview(&self, args: IndexArgs) -> Result<FeePreview, ApiError> {
        let keys = self.identity_keys();
        let current = self.profile_key.clone();
        self.database
            .call(move |db| retry_preview_tx(db, args, keys, current))
            .await
            .map_err(db_error)
    }
}

/// Match retry_tx's saved target selection without loading source files or
/// creating new authorization. Explicit spaces restrict jobs, while the current
/// profile's failed admissions and current bootstrap groups remain retryable.
fn retry_preview_tx(
    db: &Connection,
    args: IndexArgs,
    compatible_keys: Vec<String>,
    current: Option<String>,
) -> Result<FeePreview, StorageError> {
    let explicit = args.space_id.is_some();
    let mut keys = compatible_keys.clone();
    let mut spaces = HashSet::new();
    if let Some(id) = args.space_id {
        let space = load_space(db, &id)?.ok_or_else(|| {
            StorageError::from(err(ErrorCode::TargetUnavailable, "Unknown index space"))
        })?;
        keys.retain(|key| space.splitter == IndexKeys::new(key).splitter());
        if keys.is_empty() {
            return Err(StorageError::from(err(
                ErrorCode::InvalidState,
                "Select the corresponding embedding/split_chars configuration before retrying this space",
            )));
        }
        spaces.insert(id);
    } else {
        if let Some(id) = meta(db, IndexKeys::ACTIVE_SPACE)? {
            spaces.insert(id);
        }
        for key in &keys {
            if let Some(group) = meta(db, &IndexKeys::new(key).rebuild())?
                && let Some(id) = meta(db, &IndexKeys::new(key).group(&group))?
            {
                spaces.insert(id);
            }
            if let Some(id) = meta(db, &IndexKeys::new(key).group("active"))? {
                spaces.insert(id);
            }
        }
    }
    let mut result = FeePreview::default();
    let mut sources = HashSet::new();
    let mut unavailable = HashSet::new();
    for id in spaces {
        let space = load_space(db, &id)?.ok_or_else(|| {
            StorageError::from(err(ErrorCode::TargetUnavailable, "Unknown index space"))
        })?;
        if !compatible_keys
            .iter()
            .any(|key| space.splitter == IndexKeys::new(key).splitter())
        {
            if !explicit {
                continue;
            }
            return Err(StorageError::from(err(
                ErrorCode::InvalidState,
                "Index space belongs to another configuration",
            )));
        }
        if !matches!(
            space.state.as_str(),
            "active" | "paused" | "candidate" | "candidate_paused"
        ) {
            return Err(StorageError::from(err(
                ErrorCode::InvalidState,
                "Activate the corresponding old space before retrying it",
            )));
        }
        let mut q = db.prepare("SELECT m.value FROM embedding_job j JOIN app_meta m ON m.key='index.input.'||j.id WHERE j.space_id=?1 AND j.state='failed'")?;
        for row in q.query_map([&id], |r| r.get::<_, String>(0))? {
            let work: Work = decode_json(&row?)?;
            sources.insert(work.witness.node_id);
            result.segments += 1;
            result.characters += work.segment.text.chars().count() as u64;
        }
    }
    for key in &keys {
        let mut q = db.prepare("SELECT value FROM app_meta WHERE key LIKE ?1")?;
        for row in q.query_map([IndexKeys::new(key).bootstrap_pattern()], |r| {
            r.get::<_, String>(0)
        })? {
            let work: Work = decode_json(&row?)?;
            if work.state == "failed" && group_current(db, IndexKeys::new(key), &work.group)? {
                sources.insert(work.witness.node_id);
                result.segments += 1;
                result.characters += work.segment.text.chars().count() as u64;
            }
        }
        if current.as_ref() == Some(key) {
            for row in q.query_map([IndexKeys::new(key).auto_pattern()], |r| {
                r.get::<_, String>(0)
            })? {
                let admission: AutoAdmission = decode_json(&row?)?;
                if admission.state == "failed" {
                    sources.insert(admission.node_id.clone());
                    unavailable.insert(admission.node_id);
                }
            }
        }
    }
    result.sources = sources.len() as u64;
    result.unavailable = unavailable.len() as u64;
    Ok(result)
}
