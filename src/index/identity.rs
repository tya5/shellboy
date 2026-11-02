//! Configured profile identity and explicit space/retry selection.
use super::*;

/// Frozen successful-request identity, including the authorized splitter key.
/// It owns no budget/state and never derives a new authorization.
pub(super) struct EmbeddingIdentity {
    pub(super) key: String,
    pub(super) profile: String,
    pub(super) model: String,
    pub(super) query_key: String,
    pub(super) requested_dimensions: Option<usize>,
}
impl EmbeddingIdentity {
    pub(super) fn configured(config: &Config, key: String) -> Self {
        Self {
            key,
            requested_dimensions: None,
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
        let changed = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let mut pause_keys = compatible_keys
                    .iter()
                    .map(|key| IndexKeys::new(key).pause())
                    .collect::<Vec<_>>();
                if let Some(family) = &family {
                    pause_keys.push(IndexKeys::new(family).pause());
                }
                let mut paused = false;
                for key in &pause_keys {
                    paused |= meta(&tx, key)?.is_some();
                }
                if paused {
                    for key in pause_keys {
                        tx.execute("DELETE FROM app_meta WHERE key=?1", [key])?;
                    }
                    tx.commit()?;
                    return Ok(true);
                }
                let mut changed = dimensions::clear_failure(&tx, IndexKeys::new(&current_key))?;
                changed |= retry_tx(&tx, args, current_key, compatible_keys, family, filter)?;
                tx.commit()?;
                Ok(changed)
            })
            .await
            .map_err(db_error)?;
        self.publish_saved(changed);
        self.publish_pause(changed);
        self.wake();
        Ok(())
    }

    pub async fn select_space(&self, space_id: &str) -> Result<(), ApiError> {
        self.key()?;
        let id = space_id.to_owned();
        let keys = self.identity_keys();
        let config = self.config.clone();
        let changed = self.database
            .call(move |db| {
                let tx = db.transaction()?;
                let changed = select_space_tx(&tx, &id, &keys)?;
                let space = load_space(&tx, &id)?.ok_or_else(serde_error)?;
                let marker = meta(&tx, &IndexKeys::query_profile(&id))?;
                query_profile_key(&config, &space.profile, &space.model, marker.as_deref().unwrap_or(""))
                    .ok_or_else(|| StorageError::from(err(ErrorCode::InvalidState,
                        "Selected index space query fingerprint is missing or incompatible; explicitly rebuild with the intended embedding configuration")))?;
                tx.commit()?;
                Ok(changed)
            })
            .await
            .map_err(db_error)?;
        self.publish_saved(changed);
        self.spaces_changed();
        self.wake();
        Ok(())
    }

    pub async fn status(&self, args: IndexArgs) -> Result<IndexStatus, ApiError> {
        let keys = self.identity_keys();
        let family = self.endpoint_family_key.clone();
        let auto_history = self.config.indexing.auto_history;
        let mut status = self
            .database
            .call(move |db| read_status(db, args, keys, family, auto_history))
            .await
            .map_err(db_error)?;
        if let Some(reason) = self.storage_stop_reason() {
            status.paused = true;
            status.reason = Some(match status.reason.take() {
                Some(saved) => format!("{saved}; {reason}"),
                None => reason,
            });
        }
        let keys = self.identity_keys();
        let family = self.endpoint_family_key.clone();
        self.database
            .call(move |db| {
                observations::enrich(db, &mut status, &keys, family.as_deref())?;
                Ok(status)
            })
            .await
            .map_err(db_error)
    }
    pub(super) async fn next_delay(&self) -> Result<Option<Duration>, ApiError> {
        let keys = self.identity_keys();
        let family = self.failure_key().map(str::to_owned);
        let auto_history = self.config.indexing.auto_history;
        // Deferred at the step budget: look again on the next round.
        Ok(self
            .background_unit(NEXT_DELAY_UNIT, move |db, _| {
                read_next_delay(db, keys, family, auto_history)
            })
            .await?
            .unwrap_or(Some(Duration::ZERO)))
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
) -> Result<bool, StorageError> {
    let embedding_job_sql =
        "SELECT id,source_node_id FROM embedding_job WHERE space_id=?1 AND state='failed'";
    let embedding_job_sql_2 = "UPDATE embedding_job SET state='pending',attempts=0,error_json=NULL,next_attempt_at=NULL WHERE id=?1";
    let index_space_sql = "UPDATE index_space SET state=CASE WHEN state='candidate_paused' THEN 'candidate' WHEN state='paused' THEN 'active' ELSE state END WHERE id=?1 AND state IN ('candidate_paused','paused')";
    let mut changed = false;
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
                changed |= tx.execute(embedding_job_sql_2, [&job])? > 0;
                if let Some(mut work) = json_meta::<Work>(tx, &IndexKeys::input(&job))? {
                    work.authorization = "explicit-retry".into();
                    set_json_meta(tx, &IndexKeys::input(&job), &work)?;
                    tx.execute("UPDATE embedding_job SET authorized_watermark=json_set(authorized_watermark,'$.authorization','explicit-retry') WHERE id=?1",[job])?;
                }
            }
        }
        changed |= tx.execute(index_space_sql, [space_id])? > 0;
    }
    for key in &compatible_keys {
        changed |= tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::new(key).pause()],
        )? > 0;
    }
    for key in &keys {
        changed |= tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::new(key).common_failures()],
        )? > 0;
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
                    changed |= update_json_meta(tx, &record, &admission)? > 0;
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
                work.authorization = "explicit-retry".into();
                work.attempts = 0;
                work.next_at = None;
                work.error = None;
                changed |= update_json_meta(tx, &record, &work)? > 0;
            }
        }
    } // The endpoint family pause is shared even for an explicit current space.
    if let Some(family) = family {
        changed |= tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::new(&family).pause()],
        )? > 0;
        changed |= tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::new(&family).common_failures()],
        )? > 0;
    }
    Ok(changed)
}
fn select_space_tx(tx: &Transaction<'_>, id: &str, keys: &[String]) -> Result<bool, StorageError> {
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
    let mut changed = tx.execute(
        "UPDATE index_space SET state='inactive' WHERE state IN ('active','paused') AND id<>?1",
        [id],
    )? > 0;
    changed |= tx.execute(
        "UPDATE index_space SET state='active' WHERE id=?1 AND state<>'active'",
        [&id],
    )? > 0;
    changed |= store::set_meta_changed(tx, IndexKeys::ACTIVE_SPACE, id)?;
    Ok(changed)
}
fn read_status(
    db: &Connection,
    args: IndexArgs,
    keys: Vec<String>,
    family: Option<String>,
    auto_history: bool,
) -> Result<IndexStatus, StorageError> {
    let embedding_job_sql = "SELECT source_node_id,error_json FROM embedding_job WHERE space_id=?1 AND state='failed' ORDER BY id LIMIT 1";
    let explicit = args.space_id.is_some();
    let selected = args.space_id.or(meta(db, IndexKeys::ACTIVE_SPACE)?);
    let mut status = IndexStatus {
        space_id: selected.clone(),
        auto_history,
        held_auto_history: 0,
        pending: 0,
        running: 0,
        done: 0,
        failed: 0,
        paused: false,
        reason: None,
        paused_at: None,
        pause_id: None,
        spaces: Vec::new(),
        spaces_total: 0,
        spaces_truncated: false,
        probes: Vec::new(),
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
                    "pending" if !auto_history => status.held_auto_history += 1,
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
                        "pending"
                            if !auto_history && work.authorization.starts_with("new-finalized") =>
                        {
                            status.held_auto_history += 1
                        }
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
        let mut statement = db.prepare(
            "SELECT state,CASE WHEN NOT ?2 AND state='pending' THEN COALESCE(json_extract(authorized_watermark,'$.authorization'),'') LIKE 'new-finalized%' ELSE 0 END AS held,COUNT(*) FROM embedding_job WHERE space_id=?1 GROUP BY state,held",
        )?;
        for row in statement.query_map(params![&space, auto_history], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })? {
            let (state, held, count) = row?;
            match state.as_str() {
                "pending" if held => status.held_auto_history += count as u64,
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
/// Pending jobs examined per eligible space before a short conservative delay.
const NEXT_DELAY_SCAN: i64 = 256;
const NEXT_DELAY_SPACES: &str =
    "SELECT id FROM index_space WHERE state IN ('active','candidate') AND splitter_version=?1";
// jobs_ready order; the authorization test runs only on the rows actually stepped.
const NEXT_DELAY_JOBS: &str = "SELECT next_attempt_at,?2 OR COALESCE(json_extract(authorized_watermark,'$.authorization'),'') NOT LIKE 'new-finalized%' FROM embedding_job WHERE space_id=?1 AND state='pending' AND next_attempt_at IS NOT NULL ORDER BY next_attempt_at LIMIT ?3";
const NEXT_DELAY_BOOTSTRAP: &str = "SELECT value FROM app_meta WHERE key>=?1 AND key<?2";

const NEXT_DELAY_UNIT: std::num::NonZeroU32 =
    std::num::NonZeroU32::new(NEXT_DELAY_SCAN as u32).unwrap();

fn read_next_delay(
    db: &Connection,
    keys: Vec<String>,
    family: Option<String>,
    auto_history: bool,
) -> Result<Option<Duration>, StorageError> {
    // `None`: nothing is scheduled (or the profile is paused); the worker waits
    // only for a wake (enqueue, authorization, retry, select_space) (#171).
    if let Some(family) = family
        && meta(db, &IndexKeys::new(&family).pause())?.is_some()
    {
        return Ok(None);
    }
    // claim() is idle for a paused profile, so its queues need no scan.
    let mut unpaused = Vec::new();
    for key in keys {
        if meta(db, &IndexKeys::new(&key).pause())?.is_none() {
            unpaused.push(key);
        }
    }
    let keys = unpaused;
    let Some(first) = keys.first() else {
        return Ok(None);
    };
    let spaces = db
        .prepare(NEXT_DELAY_SPACES)?
        .query_map([IndexKeys::new(first).splitter()], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let mut jobs = db.prepare(NEXT_DELAY_JOBS)?;
    let mut next: Option<String> = None;
    let mut bounded = None;
    for space in spaces {
        let mut rows = jobs.query(params![space, auto_history, NEXT_DELAY_SCAN])?;
        let (mut seen, mut last) = (0, None);
        while let Some(row) = rows.next()? {
            let at: String = row.get(0)?;
            if row.get(1)? {
                next = Some(next.map_or(at.clone(), |old: String| old.min(at)));
                break;
            }
            seen += 1;
            last = Some(at);
        }
        if seen == NEXT_DELAY_SCAN {
            // An unseen match is not earlier than the last examined row; never
            // wait longer than the unscheduled recheck or spin on due rows.
            let now = chrono::Utc::now();
            let at = last
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok())
                .map_or(now, |at| at.with_timezone(&chrono::Utc))
                .clamp(
                    now + chrono::Duration::seconds(1),
                    now + chrono::Duration::seconds(30),
                )
                .fixed_offset();
            bounded = Some(bounded.map_or(at, |old: chrono::DateTime<_>| old.min(at)));
        }
    }
    let mut earliest = next.and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok());
    if let Some(at) = bounded {
        earliest = Some(earliest.map_or(at, |old| old.min(at)));
    }
    for key in keys {
        let mut statement = db.prepare(NEXT_DELAY_BOOTSTRAP)?;
        let (start, end) = IndexKeys::new(&key).bootstrap_range();
        for value in statement.query_map([start, end], |row| row.get::<_, String>(0))? {
            let work: Work = decode_json(&value?)?;
            if work.state == "pending"
                && (auto_history || !work.authorization.starts_with("new-finalized"))
                && let Some(next) = work
                    .next_at
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok())
            {
                earliest = Some(earliest.map_or(next, |old| old.min(next)));
            }
        }
    }
    Ok(earliest.map(|next| {
        (next.with_timezone(&chrono::Utc) - chrono::Utc::now())
            .to_std()
            .unwrap_or(Duration::from_millis(1))
    }))
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

    fn next_delay_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        // Synthetic jobs need no source nodes.
        db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        db
    }
    fn add_space(db: &Connection, id: &str, state: &str, key: &str) {
        db.execute(
            "INSERT INTO index_space VALUES(?1,'p','m',3,?2,?3,?4)",
            params![
                id,
                IndexKeys::new(key).splitter(),
                state,
                format!("vec_{id}")
            ],
        )
        .unwrap();
    }
    fn add_job(db: &Connection, id: &str, space: &str, at: Option<&str>, authorization: &str) {
        db.execute(
            "INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,next_attempt_at,authorized_watermark)VALUES(?1,?2,?1,0,'body','h',0,1,'pending',?3,json_object('authorization',?4))",
            params![id, space, at, authorization],
        )
        .unwrap();
    }

    fn at(seconds: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::seconds(seconds)).to_rfc3339()
    }
    fn assert_near(delay: Duration, seconds: u64) {
        let target = Duration::from_secs(seconds);
        assert!(
            delay <= target && delay + Duration::from_secs(60) > target,
            "{delay:?} is not near {target:?}"
        );
    }

    #[test]
    fn paused_profile_returns_pause_delay_without_scanning_queues() {
        // No queue tables exist, so any scan would fail instead of returning.
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        assert!(read_next_delay(&db, vec!["p".into()], None, true).is_err());
        db.execute(
            "INSERT INTO app_meta VALUES(?1,'paused')",
            [IndexKeys::new("p").pause()],
        )
        .unwrap();
        for family in [None, Some("family".to_owned())] {
            let delay = read_next_delay(&db, vec!["p".into()], family, true).unwrap();
            assert_eq!(
                delay, None,
                "a paused profile waits for resume, not a timer"
            );
        }
    }

    #[test]
    fn next_delay_picks_earliest_eligible_job_or_bootstrap() {
        let db = next_delay_db();
        add_space(&db, "active", "active", "p");
        add_space(&db, "candidate", "candidate", "p");
        add_space(&db, "paused", "paused", "p");
        add_space(&db, "other", "active", "q");
        add_job(&db, "a1", "paused", Some(&at(100)), "explicit");
        add_job(&db, "a2", "other", Some(&at(200)), "explicit");
        add_job(&db, "a3", "active", Some(&at(3600)), "new-finalized:x");
        add_job(&db, "a4", "candidate", Some(&at(7200)), "explicit");
        add_job(&db, "a5", "active", Some(&at(9000)), "explicit");
        add_job(&db, "a6", "active", None, "explicit");
        let keys = || vec!["p".to_owned()];
        assert_near(
            read_next_delay(&db, keys(), None, true).unwrap().unwrap(),
            3600,
        );
        assert_near(
            read_next_delay(&db, keys(), None, false).unwrap().unwrap(),
            7200,
        );
        let mut work = Work::pending(
            Witness {
                node_id: "n".into(),
                revision: 0,
                body_hash: None,
                raw: None,
            },
            "body".into(),
            Segment {
                start: 0,
                end: 1,
                text: "x".into(),
            },
            "g",
            "new-finalized:x",
        );
        work.next_at = Some(at(1800));
        let put = |digest: &str, work: &Work| {
            db.execute(
                "INSERT INTO app_meta VALUES(?1,?2)",
                params![
                    IndexKeys::new("p").bootstrap(digest),
                    encode_json(work).unwrap()
                ],
            )
            .unwrap();
        };
        put("b1", &work);
        work.authorization = "explicit".into();
        work.next_at = Some(at(5400));
        put("b2", &work);
        assert_near(
            read_next_delay(&db, keys(), None, true).unwrap().unwrap(),
            1800,
        );
        assert_near(
            read_next_delay(&db, keys(), None, false).unwrap().unwrap(),
            5400,
        );
    }

    #[test]
    fn next_delay_scan_bound_falls_back_conservatively() {
        let db = next_delay_db();
        add_space(&db, "space", "active", "p");
        add_job(&db, "late", "space", Some(&at(20)), "explicit");
        for n in 0..NEXT_DELAY_SCAN - 1 {
            add_job(
                &db,
                &format!("f{n:04}"),
                "space",
                Some(&at(10)),
                "new-finalized:x",
            );
        }
        let keys = || vec!["p".to_owned()];
        // Within the bound the exact earliest eligible job is chosen.
        let delay = read_next_delay(&db, keys(), None, false).unwrap().unwrap();
        assert!(delay > Duration::from_secs(10) && delay <= Duration::from_secs(20));
        add_job(&db, "f-last", "space", Some(&at(10)), "new-finalized:x");
        // At the bound the last examined row is a lower bound for any match.
        let delay = read_next_delay(&db, keys(), None, false).unwrap().unwrap();
        assert!(delay > Duration::from_secs(1) && delay <= Duration::from_secs(10));
        db.execute(
            "UPDATE embedding_job SET next_attempt_at=?1 WHERE id LIKE 'f%'",
            [at(3600)],
        )
        .unwrap();
        db.execute(
            "UPDATE embedding_job SET next_attempt_at=?1 WHERE id='late'",
            [at(7200)],
        )
        .unwrap();
        // Never later than the 30 s recheck used when nothing is scheduled.
        let delay = read_next_delay(&db, keys(), None, false).unwrap().unwrap();
        assert!(delay > Duration::from_secs(20) && delay <= Duration::from_secs(30));
        db.execute(
            "UPDATE embedding_job SET next_attempt_at=?1 WHERE id LIKE 'f%'",
            [at(-60)],
        )
        .unwrap();
        // Past ineligible rows never make the worker spin.
        let delay = read_next_delay(&db, keys(), None, false).unwrap().unwrap();
        assert!(delay > Duration::ZERO && delay <= Duration::from_secs(1));
    }

    #[test]
    fn bootstrap_range_selects_exactly_the_profile_prefix() {
        let db = next_delay_db();
        let profile = "Pro_f%il:日本";
        let keys = [
            format!("index.bootstrap.{profile}."),
            format!("index.bootstrap.{profile}.a"),
            format!("index.bootstrap.{profile}.%_Z"),
            format!("index.bootstrap.{profile}"),
            format!("index.bootstrap.{profile}/"),
            format!("index.bootstrap.{profile}x.a"),
            "index.bootstrap.pro_f%il:日本.a".to_owned(),
            "index.bootstrap.ProXfYYil:日本.a".to_owned(),
            "index.bootstrap.Pro_f%il:日本-.a".to_owned(),
        ];
        for key in &keys {
            db.execute("INSERT INTO app_meta VALUES(?1,'v')", [key])
                .unwrap();
        }
        let (start, end) = IndexKeys::new(profile).bootstrap_range();
        let mut q = db
            .prepare("SELECT key FROM app_meta WHERE key>=?1 AND key<?2 ORDER BY key")
            .unwrap();
        let ranged: Vec<String> = q
            .query_map([start, end], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let prefix = format!("index.bootstrap.{profile}.");
        let mut exact: Vec<String> = keys
            .iter()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        exact.sort();
        assert_eq!(ranged, exact);
        let mut q = db
            .prepare("SELECT key FROM app_meta WHERE key LIKE ?1")
            .unwrap();
        let liked: Vec<String> = q
            .query_map([IndexKeys::new(profile).bootstrap_pattern()], |row| {
                row.get(0)
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(ranged.iter().all(|key| liked.contains(key)));
        assert!(
            liked.len() > ranged.len(),
            "LIKE also matched other profiles"
        );
        let mut plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {NEXT_DELAY_BOOTSTRAP}"))
            .unwrap();
        let detail: String = plan.query_row(["a", "b"], |row| row.get(3)).unwrap();
        assert!(detail.contains("USING INDEX"), "{detail}");
    }

    /// Synthetic #164-sized measurement; prints plans and times, asserts no timing.
    #[test]
    #[ignore = "manual measurement: cargo test --lib next_delay_synthetic_measurement -- --ignored --nocapture"]
    fn next_delay_synthetic_measurement() {
        let db = next_delay_db();
        add_space(&db, "space", "active", "p");
        let far = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        let tx = db.unchecked_transaction().unwrap();
        let filler = "x".repeat(1800);
        for n in 0..84_948 {
            add_job(
                &tx,
                &format!("job{n:06}"),
                "space",
                Some(&far),
                "new-finalized:auto",
            );
            tx.execute(
                "INSERT INTO app_meta VALUES(?1,?2)",
                params![IndexKeys::input(&format!("job{n:06}")), filler],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        db.execute_batch("ANALYZE").unwrap();
        for sql in [
            "SELECT MIN(j.next_attempt_at) FROM embedding_job j JOIN index_space s ON s.id=j.space_id WHERE j.state='pending' AND j.next_attempt_at IS NOT NULL AND s.state IN ('active','candidate') AND s.splitter_version=?1 AND (?2 OR COALESCE(json_extract(j.authorized_watermark,'$.authorization'),'') NOT LIKE 'new-finalized%')",
            "SELECT value FROM app_meta WHERE key LIKE ?1",
            NEXT_DELAY_SPACES,
            NEXT_DELAY_JOBS,
            NEXT_DELAY_BOOTSTRAP,
        ] {
            let mut q = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let plan: Vec<String> = q
                .query_map(
                    rusqlite::params_from_iter(std::iter::repeat_n(
                        rusqlite::types::Value::Null,
                        q.parameter_count(),
                    )),
                    |row| row.get(3),
                )
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            println!("{sql}\n  {plan:?}");
        }
        let family = Some("family".to_owned());
        for (label, family, auto) in [
            ("unpaused auto_history=true", family.clone(), true),
            ("unpaused auto_history=false", family.clone(), false),
        ] {
            let start = std::time::Instant::now();
            let delay = read_next_delay(&db, vec!["p".into()], family, auto).unwrap();
            println!("{label}: {:?} -> {delay:?}", start.elapsed());
        }
        db.execute(
            "INSERT INTO app_meta VALUES(?1,'paused')",
            [IndexKeys::new("p").pause()],
        )
        .unwrap();
        for (label, family) in [
            ("paused family=None", None),
            ("paused family unpaused", family),
        ] {
            let start = std::time::Instant::now();
            let delay = read_next_delay(&db, vec!["p".into()], family, false).unwrap();
            println!("{label}: {:?} -> {delay:?}", start.elapsed());
        }
    }
}
