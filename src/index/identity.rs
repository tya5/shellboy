//! Configured profile identity and explicit space/retry selection.
use super::*;

/// Frozen successful-request identity, including the authorized splitter key.
/// It owns no budget/state and never derives a new authorization.
pub(super) struct EmbeddingIdentity {
    pub(super) key: String,
    pub(super) profile: String,
    pub(super) model: String,
    pub(super) query_key: String,
}
impl EmbeddingIdentity {
    pub(super) fn configured(config: &Config, key: String) -> Self {
        Self {
            key,
            query_key: endpoint_family_key(config).expect("configured embedding identity"),
            profile: config.embedding.connection.clone().unwrap_or_default(),
            model: config.embedding.model.clone().unwrap_or_default(),
        }
    }
    pub(super) fn keys(&self) -> IndexKeys<'_> {
        IndexKeys::new(&self.key)
    }
}

pub(super) fn endpoint_family_key(config: &Config) -> Option<String> {
    config
        .embedding
        .connection
        .as_ref()
        .zip(config.embedding.model.as_ref())
        .and_then(|(name, model)| {
            config
                .connections
                .get(name)
                .map(|connection| endpoint_key(name, &connection.base_url, model))
        })
}
pub(super) fn profile_key(config: &Config) -> Option<String> {
    endpoint_family_key(config).map(|family| split_key(&family, config.indexing.split_chars))
}
fn endpoint_key(name: &str, endpoint: &str, model: &str) -> String {
    hash(format!("{name}\0{endpoint}\0{model}\0{SPLITTER}").as_bytes())
}
fn split_key(family: &str, split_chars: usize) -> String {
    hash(format!("split-chars-v1\0{family}\0{split_chars}").as_bytes())
}
/// Query identity is mandatory and independent of source segmentation policy.
/// Missing markers require an explicit rebuild, never read-time repair.
pub(super) fn query_profile_key(
    config: &Config,
    name: &str,
    model: &str,
    stored: &str,
) -> Option<String> {
    let connection = config.connections.get(name)?;
    let expected = endpoint_key(name, &connection.base_url, model);
    (stored == expected).then_some(expected)
}

pub(super) fn bind_query_profile(
    tx: &Transaction<'_>,
    space: &str,
    expected: &str,
) -> Result<(), StorageError> {
    let key = IndexKeys::query_profile(space);
    match meta(tx, &key)? {
        Some(stored) if stored == expected => Ok(()),
        Some(_) => Err(StorageError::from(err(
            ErrorCode::InvalidState,
            "Selected index space endpoint/model fingerprint does not match current configuration",
        ))),
        None => set_meta(tx, &key, expected),
    }
}

impl IndexService {
    pub(super) fn identity_keys(&self) -> Vec<String> {
        self.profile_key.iter().cloned().collect()
    }
    pub(super) fn failure_key(&self) -> Option<&str> {
        self.endpoint_family_key.as_deref()
    }

    pub(super) async fn write_space(&self, args: &IndexArgs) -> Result<Option<String>, ApiError> {
        let explicit = args.space_id.clone();
        let expected = self
            .profile_key
            .as_ref()
            .map(|key| IndexKeys::new(key).splitter());
        self.database
            .call(move |db| read_write_space(db, explicit, expected))
            .await
            .map_err(db_error)
    }

    pub async fn retry(&self, args: IndexArgs) -> Result<(), ApiError> {
        self.key()?;
        let current_key = self.profile_key.clone().unwrap();
        let compatible_keys = self.identity_keys();
        let family = self.failure_key().map(str::to_owned);
        let filter =
            if args.scope.is_some() || !args.source_ids.is_empty() || !args.thread_ids.is_empty() {
                Some(
                    self.selected_sources(&args)
                        .await?
                        .into_iter()
                        .collect::<HashSet<_>>(),
                )
            } else {
                None
            };
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                retry_tx(&tx, args, current_key, compatible_keys, family, filter)?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(db_error)?;
        self.wake();
        Ok(())
    }

    pub async fn select_space(&self, space_id: &str) -> Result<(), ApiError> {
        self.key()?;
        let id = space_id.to_owned();
        let keys = self.identity_keys();
        let config = self.config.clone();
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                select_space_tx(&tx, &id, &keys)?;
                let space = load_space(&tx, &id)?.ok_or_else(serde_error)?;
                let marker = meta(&tx, &IndexKeys::query_profile(&id))?;
                query_profile_key(&config, &space.profile, &space.model, marker.as_deref().unwrap_or(""))
                    .ok_or_else(|| StorageError::from(err(ErrorCode::InvalidState,
                        "Selected index space query fingerprint is missing or incompatible; explicitly rebuild with the intended embedding configuration")))?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(db_error)?;
        self.wake();
        Ok(())
    }

    pub async fn status(&self, args: IndexArgs) -> Result<IndexStatus, ApiError> {
        let keys = self.identity_keys();
        let family = self.endpoint_family_key.clone();
        let mut status = self
            .database
            .call(move |db| read_status(db, args, keys, family))
            .await
            .map_err(db_error)?;
        if let Some(reason) = self.storage_stop_reason() {
            status.paused = true;
            status.reason = Some(match status.reason.take() {
                Some(saved) => format!("{saved}; {reason}"),
                None => reason,
            });
        }
        Ok(status)
    }
    pub(super) async fn next_delay(&self) -> Result<Duration, ApiError> {
        let keys = self.identity_keys();
        let family = self.failure_key().map(str::to_owned);
        self.database
            .call(move |db| read_next_delay(db, keys, family))
            .await
            .map_err(db_error)
    }
}

fn read_write_space(
    db: &Connection,
    explicit: Option<String>,
    expected: Option<String>,
) -> Result<Option<String>, StorageError> {
    let active = meta(db, IndexKeys::ACTIVE_SPACE)?;
    let selected = explicit.clone().or_else(|| active.clone());
    let Some(id) = selected else {
        return Ok(None);
    };
    let space = load_space(db, &id)?.ok_or_else(|| {
        StorageError::from(err(ErrorCode::TargetUnavailable, "Unknown index space"))
    })?;
    let compatible = expected.as_deref() == Some(space.splitter.as_str());
    if explicit.is_some() && (!compatible || active.as_deref() != Some(id.as_str())) {
        return Err(StorageError::from(err(
            ErrorCode::InvalidState,
            "Select the space with its matching embedding/split_chars configuration, or omit space to prepare a new current-policy index",
        )));
    }
    Ok(compatible.then_some(id))
}
fn retry_tx(
    tx: &Transaction<'_>,
    args: IndexArgs,
    current_key: String,
    compatible_keys: Vec<String>,
    family: Option<String>,
    filter: Option<HashSet<String>>,
) -> Result<(), StorageError> {
    let embedding_job_sql =
        "SELECT id,source_node_id FROM embedding_job WHERE space_id=?1 AND state='failed'";
    let embedding_job_sql_2 = "UPDATE embedding_job SET state='pending',attempts=0,error_json=NULL,next_attempt_at=NULL WHERE id=?1";
    let index_space_sql = "UPDATE index_space SET state=CASE WHEN state='candidate_paused' THEN 'candidate' WHEN state='paused' THEN 'active' ELSE state END WHERE id=?1";
    let explicit = args.space_id.is_some();
    let mut keys = compatible_keys.clone();
    let mut spaces = HashSet::new();
    if let Some(space_id) = args.space_id {
        let space = load_space(tx, &space_id)?.ok_or_else(|| {
            StorageError::from(err(ErrorCode::TargetUnavailable, "Unknown index space"))
        })?;
        keys.retain(|key| space.splitter == IndexKeys::new(key).splitter());
        if keys.is_empty() {
            return Err(StorageError::from(err(
                ErrorCode::InvalidState,
                "Select the corresponding embedding/split_chars configuration before retrying this space",
            )));
        }
        spaces.insert(space_id);
    } else {
        if let Some(space) = meta(tx, IndexKeys::ACTIVE_SPACE)? {
            spaces.insert(space);
        }
        for key in &keys {
            if let Some(group) = meta(tx, &IndexKeys::new(key).rebuild())?
                && let Some(space) = meta(tx, &IndexKeys::new(key).group(&group))?
            {
                spaces.insert(space);
            }
            if let Some(space) = meta(tx, &IndexKeys::new(key).group("active"))? {
                spaces.insert(space);
            }
        }
    }
    for space_id in spaces {
        let space = load_space(tx, &space_id)?.ok_or_else(|| {
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
        let mut statement = tx.prepare(embedding_job_sql)?;
        let rows = statement
            .query_map([&space_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for (job, node) in rows {
            if filter.as_ref().is_none_or(|ids| ids.contains(&node))
                && meta(tx, &IndexKeys::input(&job))?.is_some()
            {
                tx.execute(embedding_job_sql_2, [job])?;
            }
        }
        tx.execute(index_space_sql, [space_id])?;
    }
    for key in &compatible_keys {
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::new(key).pause()],
        )?;
    }
    for key in &keys {
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::new(key).common_failures()],
        )?;
        if key == &current_key {
            let mut statement = tx.prepare("SELECT key,value FROM app_meta WHERE key LIKE ?1")?;
            let rows = statement
                .query_map([IndexKeys::new(key).auto_pattern()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            for (record, value) in rows {
                let mut admission: AutoAdmission = decode_json(&value)?;
                if admission.state == "failed"
                    && filter
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&admission.node_id))
                {
                    admission.state = "pending".into();
                    admission.error = None;
                    update_json_meta(tx, &record, &admission)?;
                }
            }
        }
        let mut statement = tx.prepare("SELECT key,value FROM app_meta WHERE key LIKE ?1")?;
        let rows = statement
            .query_map([IndexKeys::new(key).bootstrap_pattern()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for (record, value) in rows {
            let mut work: Work = decode_json(&value)?;
            if work.state == "failed"
                && group_current(tx, IndexKeys::new(key), &work.group)?
                && filter
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&work.witness.node_id))
            {
                work.state = "pending".into();
                work.attempts = 0;
                work.next_at = None;
                work.error = None;
                update_json_meta(tx, &record, &work)?;
            }
        }
    } // The endpoint family pause is shared even for an explicit current space.
    if let Some(family) = family {
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::new(&family).pause()],
        )?;
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::new(&family).common_failures()],
        )?;
    }
    Ok(())
}
fn select_space_tx(tx: &Transaction<'_>, id: &str, keys: &[String]) -> Result<(), StorageError> {
    let space = load_space(tx, id)?.ok_or_else(|| {
        StorageError::from(err(ErrorCode::TargetUnavailable, "Unknown index space"))
    })?;
    if !keys
        .iter()
        .any(|key| space.splitter == IndexKeys::new(key).splitter())
    {
        return Err(StorageError::from(err(
            ErrorCode::InvalidState,
            "Select the corresponding embedding/split_chars configuration before activating this old space",
        )));
    }
    if !matches!(space.state.as_str(), "active" | "inactive" | "paused") {
        return Err(StorageError::from(err(
            ErrorCode::InvalidState,
            "Incomplete candidate cannot become active",
        )));
    }
    tx.execute(
        "UPDATE index_space SET state='inactive' WHERE state IN ('active','paused')",
        [],
    )?;
    tx.execute("UPDATE index_space SET state='active' WHERE id=?1", [&id])?;
    set_meta(tx, IndexKeys::ACTIVE_SPACE, id)?;
    Ok(())
}
fn read_status(
    db: &Connection,
    args: IndexArgs,
    keys: Vec<String>,
    family: Option<String>,
) -> Result<IndexStatus, StorageError> {
    let embedding_job_sql = "SELECT source_node_id,error_json FROM embedding_job WHERE space_id=?1 AND state='failed' ORDER BY id LIMIT 1";
    let explicit = args.space_id.is_some();
    let selected = args.space_id.or(meta(db, IndexKeys::ACTIVE_SPACE)?);
    let mut status = IndexStatus {
        space_id: selected.clone(),
        pending: 0,
        running: 0,
        done: 0,
        failed: 0,
        paused: false,
        reason: None,
    };
    let mut spaces = HashSet::new();
    if let Some(space) = &selected {
        spaces.insert(space.clone());
    }
    if let Some(family) = family
        && let Some(pause) = meta(db, &IndexKeys::new(&family).pause())?
    {
        status.paused = true;
        status.reason = Some(pause);
    }
    for key in &keys {
        if let Some(pause) = meta(db, &IndexKeys::new(key).pause())? {
            status.paused = true;
            if status.reason.is_none() {
                status.reason = Some(pause);
            }
        }
        if !explicit {
            if let Some(group) = meta(db, &IndexKeys::new(key).rebuild())? {
                if let Some(space) = meta(db, &IndexKeys::new(key).group(&group))? {
                    spaces.insert(space);
                }
                if status.reason.is_none()
                    && meta(db, &IndexKeys::new(key).enqueueing(&group))?.is_some()
                {
                    status.reason=Some("Rebuild source admission is in progress or was interrupted; explicitly rebuild after interrupted preparation".into());
                }
            }
            if let Some(space) = meta(db, &IndexKeys::new(key).group("active"))? {
                spaces.insert(space);
            }
            let mut statement = db.prepare("SELECT value FROM app_meta WHERE key LIKE ?1")?;
            for value in statement.query_map([IndexKeys::new(key).auto_pattern()], |row| {
                row.get::<_, String>(0)
            })? {
                let admission: AutoAdmission = decode_json(&value?)?;
                match admission.state.as_str() {
                    "pending" => status.pending += 1,
                    "running" => status.running += 1,
                    "failed" => status.failed += 1,
                    _ => {}
                }
                if status.reason.is_none() {
                    status.reason = admission
                        .error
                        .map(|error| format!("Source {}: {error}", admission.node_id));
                }
            }
            let mut statement = db.prepare("SELECT value FROM app_meta WHERE key LIKE ?1")?;
            for value in statement.query_map([IndexKeys::new(key).bootstrap_pattern()], |row| {
                row.get::<_, String>(0)
            })? {
                let work: Work = decode_json(&value?)?;
                if group_current(db, IndexKeys::new(key), &work.group)? {
                    match work.state.as_str() {
                        "pending" => status.pending += 1,
                        "running" => status.running += 1,
                        "failed" => status.failed += 1,
                        _ => {}
                    }
                    if status.reason.is_none() {
                        status.reason = work
                            .error
                            .map(|error| format!("Source {}: {error}", work.witness.node_id));
                    }
                }
            }
        }
    }
    for space in spaces {
        let mut statement = db
            .prepare("SELECT state,COUNT(*) FROM embedding_job WHERE space_id=?1 GROUP BY state")?;
        for row in statement.query_map([&space], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (state, count) = row?;
            match state.as_str() {
                "pending" => status.pending += count as u64,
                "running" => status.running += count as u64,
                "done" => status.done += count as u64,
                "failed" => status.failed += count as u64,
                _ => {}
            }
        }
        if status.reason.is_none() {
            let failure: Option<(String, Option<String>)> = db
                .query_row(embedding_job_sql, [space], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()?;
            if let Some((node, Some(error))) = failure {
                status.reason = Some(format!("Source {node}: {error}"));
            }
        }
    }
    if keys.is_empty() {
        status.reason = Some("Embedding connection/model is not configured".into());
    }

    Ok(status)
}
fn read_next_delay(
    db: &Connection,
    keys: Vec<String>,
    family: Option<String>,
) -> Result<Duration, StorageError> {
    let embedding_job_sql = "SELECT MIN(j.next_attempt_at) FROM embedding_job j JOIN index_space s ON s.id=j.space_id WHERE j.state='pending' AND j.next_attempt_at IS NOT NULL AND s.state IN ('active','candidate') AND s.splitter_version=?1";
    if let Some(family) = family
        && meta(db, &IndexKeys::new(&family).pause())?.is_some()
    {
        return Ok(Duration::from_secs(30));
    }
    let splitters: Vec<_> = keys
        .iter()
        .map(|key| IndexKeys::new(key).splitter())
        .collect();
    let next: Option<String> =
        db.query_row(embedding_job_sql, params![splitters.first()], |row| {
            row.get(0)
        })?;
    let mut earliest = next.and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok());
    for key in keys {
        let mut statement = db.prepare("SELECT value FROM app_meta WHERE key LIKE ?1")?;
        for value in statement.query_map([IndexKeys::new(&key).bootstrap_pattern()], |row| {
            row.get::<_, String>(0)
        })? {
            let work: Work = decode_json(&value?)?;
            if work.state == "pending"
                && let Some(next) = work
                    .next_at
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok())
            {
                earliest = Some(earliest.map_or(next, |old| old.min(next)));
            }
        }
    }
    Ok(earliest
        .map(|next| {
            (next.with_timezone(&chrono::Utc) - chrono::Utc::now())
                .to_std()
                .unwrap_or(Duration::from_millis(1))
        })
        .unwrap_or(Duration::from_secs(30)))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_query_marker_is_written_once_and_matching_reads_do_not_write() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TRIGGER prevent_marker_update BEFORE UPDATE ON app_meta
            WHEN OLD.key LIKE 'index.query_profile.%'
            BEGIN SELECT RAISE(ABORT,'query marker must not be rewritten'); END;",
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        bind_query_profile(&tx, "space", "verified").unwrap();
        tx.commit().unwrap();
        let before: i64 = db
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before, 1);
        let tx = db.transaction().unwrap();
        bind_query_profile(&tx, "space", "verified").unwrap();
        let error = bind_query_profile(&tx, "space", "different")
            .unwrap_err()
            .into_api_error();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert_eq!(
            meta(&tx, &IndexKeys::query_profile("space"))
                .unwrap()
                .as_deref(),
            Some("verified")
        );
        tx.commit().unwrap();
        let after: i64 = db
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            after, before,
            "matching reads and rejected rebinding do not write"
        );
    }

    #[test]
    fn segmentation_policy_changes_identity_but_not_endpoint_failure_family() {
        let mut config = Config::default();
        config.connections.insert(
            "embed".into(),
            crate::config::Connection {
                base_url: "http://fixture/v1".into(),
                ..Default::default()
            },
        );
        config.embedding.connection = Some("embed".into());
        config.embedding.model = Some("embed-model".into());
        config.indexing.split_chars = 8;
        let old = profile_key(&config).unwrap();
        let family = endpoint_family_key(&config).unwrap();
        config.indexing.split_chars = 12;
        assert_ne!(profile_key(&config).unwrap(), old);
        assert_eq!(endpoint_family_key(&config).unwrap(), family);
        config.embedding.model = Some("other".into());
        assert_ne!(endpoint_family_key(&config).unwrap(), family);
    }
    #[test]
    fn query_identity_requires_marker_and_survives_split_policy_changes() {
        let mut config = Config::default();
        config.connections.insert(
            "embed".into(),
            crate::config::Connection {
                base_url: "http://fixture/v1".into(),
                ..Default::default()
            },
        );
        config.embedding.connection = Some("embed".into());
        config.embedding.model = Some("model".into());
        let marker = endpoint_family_key(&config).unwrap();
        assert!(query_profile_key(&config, "embed", "model", "").is_none());
        assert_eq!(
            query_profile_key(&config, "embed", "model", &marker),
            Some(marker.clone())
        );
        config.indexing.split_chars += 1;
        assert_eq!(
            query_profile_key(&config, "embed", "model", &marker),
            Some(marker.clone())
        );
        config.connections.get_mut("embed").unwrap().base_url = "http://other/v1".into();
        assert!(query_profile_key(&config, "embed", "model", &marker).is_none());
    }
}
