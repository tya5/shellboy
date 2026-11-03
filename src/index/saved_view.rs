//! Finite saved observations. Callers own transaction/admission; these never run HTTP.
use super::*;
use std::collections::BTreeMap;
const SCALAR_CELL_BYTES: usize = 512;
const SCALAR_CHANNEL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SavedReadLimits {
    pub sources: usize,
    pub source_bytes: usize,
    pub body_bytes: usize,
    pub rows: usize,
    pub metadata_bytes: usize,
    pub spaces: usize,
}
impl Default for SavedReadLimits {
    fn default() -> Self {
        Self {
            sources: 32,
            source_bytes: 128 * 1024,
            body_bytes: 512 * 1024,
            rows: 1024,
            metadata_bytes: 128 * 1024,
            spaces: 200,
        }
    }
}
impl SavedReadLimits {
    fn checked(self) -> Result<Self, ApiError> {
        if self.sources == 0
            || self.sources > 32
            || self.source_bytes == 0
            || self.source_bytes > 128 * 1024
            || self.body_bytes == 0
            || self.body_bytes > 512 * 1024
            || self.rows == 0
            || self.rows > 1024
            || self.metadata_bytes == 0
            || self.metadata_bytes > 128 * 1024
            || self.spaces == 0
            || self.spaces > 200
        {
            return Err(err(
                ErrorCode::InvalidArgument,
                "Saved index view limits exceed their finite bounds",
            ));
        }
        Ok(self)
    }
}
#[derive(Debug)]
struct MetaRow {
    key: String,
    body: Option<String>,
    present: bool,
    scalar_limited: bool,
}
#[derive(Debug)]
struct SpaceRow {
    id: String,
    profile: Option<String>,
    model: Option<String>,
    dimension: i64,
    state: String,
    requested: Option<String>,
    requested_present: bool,
    scalar_limited: bool,
}
#[derive(Debug)]
struct JobRow {
    state: String,
    automatic: Option<bool>,
    source: String,
    error: Option<String>,
    error_limited: bool,
    scalar_limited: bool,
}
#[derive(Debug)]
pub(crate) struct RawIndexSaved {
    args: IndexArgs,
    configured: bool,
    auto_history: bool,
    active: Option<String>,
    selected: Option<String>,
    spaces: Vec<SpaceRow>,
    spaces_more: bool,
    jobs: Vec<JobRow>,
    jobs_more: bool,
    chunks: Vec<String>,
    chunks_more: bool,
    meta: Vec<MetaRow>,
    work: Vec<MetaRow>,
    work_more: bool,
    probes: Vec<MetaRow>,
    probes_more: bool,
    profile: Option<String>,
    family: Option<String>,
    diagnostic: Option<String>,
    read_at: String,
}
fn validate_args(args: &IndexArgs) -> Result<(), ApiError> {
    if args.source_ids.len() > 128
        || args.thread_ids.len() > 128
        || args
            .source_ids
            .iter()
            .chain(&args.thread_ids)
            .chain(args.space_id.iter())
            .any(|id| id.len() > 4096)
    {
        return Err(err(
            ErrorCode::InputLimit,
            "Saved index observation accepts at most128 source/Thread IDs of at most4096 bytes; use the full CLI for a larger explicit scope",
        ));
    }
    Ok(())
}
fn read_meta(db: &Connection, key: String, remaining: &mut usize) -> Result<MetaRow, StorageError> {
    let row: Option<(Option<String>,)> = db.query_row(
        "SELECT CASE WHEN octet_length(value)<=?2 AND octet_length(value)<=16384 THEN value END FROM app_meta WHERE key=?1", params![key, *remaining as i64], |r| Ok((r.get(0)?,)),
    ).optional()?;
    let present = row.is_some();
    let body = row.and_then(|r| r.0);
    *remaining = remaining.saturating_sub(body.as_ref().map_or(0, String::len));
    Ok(MetaRow {
        scalar_limited: key.len() > SCALAR_CELL_BYTES,
        key,
        body,
        present,
    })
}
fn read_prefix(
    db: &Connection,
    prefix: &str,
    cap: usize,
    remaining: &mut usize,
) -> Result<(Vec<MetaRow>, bool), StorageError> {
    // All callers use an ASCII prefix ending in '.', so '/' is its exact next
    // prefix. The PK range is bounded before window/copy accounting.
    let end = format!("{}/", prefix.strip_suffix('.').ok_or_else(serde_error)?);
    let mut q = db.prepare("WITH picked AS MATERIALIZED (SELECT rowid AS row_key,CASE WHEN octet_length(key)<=512 THEN key ELSE '' END AS key,octet_length(key)>512 AS scalar_limited,octet_length(value) AS size FROM app_meta WHERE key>=?1 AND key<?2 ORDER BY app_meta.key LIMIT ?3), budgeted AS (SELECT *,SUM(CASE WHEN size<=16384 THEN size ELSE 0 END) OVER(ORDER BY key) AS used,ROW_NUMBER()OVER(ORDER BY key) AS ordinal FROM picked) SELECT CASE WHEN octet_length(b.key)<=512 THEN b.key ELSE '' END,CASE WHEN NOT b.scalar_limited AND b.ordinal<=?5 AND b.size<=16384 AND b.used<=?4 THEN m.value END,b.scalar_limited FROM budgeted b JOIN app_meta m ON m.rowid=b.row_key ORDER BY b.key")?;
    let mut rows = q
        .query_map(
            params![prefix, end, (cap + 1) as i64, *remaining as i64, cap as i64],
            |r| {
                Ok(MetaRow {
                    key: r.get(0)?,
                    body: r.get(1)?,
                    present: true,
                    scalar_limited: r.get(2)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let more = rows.len() > cap;
    rows.truncate(cap);
    *remaining = remaining.saturating_sub(
        rows.iter()
            .filter_map(|r| r.body.as_ref())
            .map(String::len)
            .sum::<usize>(),
    );
    Ok((rows, more))
}
fn read_space(db: &Connection, id: &str) -> Result<Option<SpaceRow>, StorageError> {
    db.query_row("SELECT CASE WHEN octet_length(s.id)<=512 THEN s.id ELSE '' END,CASE WHEN octet_length(s.profile_id)<=512 THEN s.profile_id END,CASE WHEN octet_length(s.model_id)<=512 THEN s.model_id END,s.dimension,CASE WHEN octet_length(s.state)<=64 THEN s.state ELSE '' END,CASE WHEN octet_length(d.value)<=64 THEN d.value END,d.key IS NOT NULL,octet_length(s.id)>512 OR octet_length(s.state)>64 FROM index_space s LEFT JOIN app_meta d ON d.key='index.space_dimensions.'||s.id WHERE s.id=?1 AND octet_length(s.id)<=512",[id],space_row).optional().map_err(StorageError::from)
}
fn space_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SpaceRow> {
    Ok(SpaceRow {
        id: r.get(0)?,
        profile: r.get(1)?,
        model: r.get(2)?,
        dimension: r.get(3)?,
        state: r.get(4)?,
        requested: r.get(5)?,
        requested_present: r.get(6)?,
        scalar_limited: r.get(7)?,
    })
}
impl IndexService {
    pub(crate) fn read_saved_tx(
        &self,
        db: &Connection,
        args: &IndexArgs,
        limits: SavedReadLimits,
    ) -> Result<RawIndexSaved, ApiError> {
        let limits = limits.checked()?;
        validate_args(args)?;
        self.read_saved_inner(db, args, limits).map_err(db_error)
    }
    fn read_saved_inner(
        &self,
        db: &Connection,
        args: &IndexArgs,
        limits: SavedReadLimits,
    ) -> Result<RawIndexSaved, StorageError> {
        let mut budget = limits.metadata_bytes;
        let active_row = read_meta(db, IndexKeys::ACTIVE_SPACE.into(), &mut budget)?;
        let active = active_row.body.clone();
        let selected = args.space_id.clone().or_else(|| active.clone());
        let mut meta_rows = vec![active_row];
        let mut current_spaces = selected.iter().cloned().collect::<Vec<_>>();
        if let Some(profile) = &self.profile_key {
            let keys = IndexKeys::new(profile);
            for key in [
                keys.pause(),
                keys.recovery_required(),
                dimensions::candidate_key(keys),
            ] {
                meta_rows.push(read_meta(db, key, &mut budget)?);
            }
            let rebuild = read_meta(db, keys.rebuild(), &mut budget)?;
            if let Some(group) = &rebuild.body {
                let candidate = read_meta(db, keys.group(group), &mut budget)?;
                if args.space_id.is_none() {
                    current_spaces.extend(candidate.body.iter().cloned());
                }
                meta_rows.push(candidate);
                meta_rows.push(read_meta(db, keys.enqueueing(group), &mut budget)?);
                meta_rows.push(read_meta(db, keys.enqueue_failed(group), &mut budget)?);
            }
            meta_rows.push(rebuild);
            let group = read_meta(db, keys.group("active"), &mut budget)?;
            if args.space_id.is_none() {
                current_spaces.extend(group.body.iter().cloned());
            }
            meta_rows.push(group);
        }
        if let Some(family) = &self.endpoint_family_key {
            meta_rows.push(read_meta(db, IndexKeys::new(family).pause(), &mut budget)?);
        }
        current_spaces.sort();
        current_spaces.dedup();
        let mut spaces = Vec::new();
        if let Some(id) = &selected {
            if let Some(space) = read_space(db, id)? {
                spaces.push(space);
            } else {
                return Err(StorageError::from(err(
                    ErrorCode::InvalidArgument,
                    "Selected index space does not exist or exceeds saved-view identity bounds",
                )));
            }
        }
        let capacity = limits.spaces.saturating_sub(spaces.len());
        let mut q=db.prepare("SELECT CASE WHEN octet_length(s.id)<=512 THEN s.id ELSE '' END,CASE WHEN octet_length(s.profile_id)<=512 THEN s.profile_id END,CASE WHEN octet_length(s.model_id)<=512 THEN s.model_id END,s.dimension,CASE WHEN octet_length(s.state)<=64 THEN s.state ELSE '' END,CASE WHEN octet_length(d.value)<=64 THEN d.value END,d.key IS NOT NULL,octet_length(s.id)>512 OR octet_length(s.state)>64 FROM index_space s LEFT JOIN app_meta d ON d.key='index.space_dimensions.'||s.id WHERE (?1 IS NULL OR s.id<>?1) ORDER BY s.rowid DESC LIMIT ?2")?;
        let mut others = q
            .query_map(params![selected, (capacity + 1) as i64], space_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let spaces_more = others.len() > capacity;
        others.truncate(capacity);
        spaces.extend(others);
        let mut q=db.prepare("WITH wanted AS (SELECT value AS id FROM json_each(?1)), picked AS MATERIALIZED (SELECT j.space_id,CASE WHEN octet_length(j.state)<=64 THEN j.state ELSE '' END AS state,octet_length(j.state)>64 AS state_limited,CASE WHEN octet_length(j.source_node_id)<=512 THEN j.source_node_id ELSE '' END AS source,j.rowid AS row_key,octet_length(j.error_json) AS size,octet_length(j.authorized_watermark) AS authorization_size FROM wanted w CROSS JOIN embedding_job j INDEXED BY jobs_ready WHERE j.space_id=w.id LIMIT ?2), budgeted AS (SELECT *,SUM(CASE WHEN size<=16384 THEN size ELSE 0 END) OVER(ORDER BY row_key) AS used,SUM(CASE WHEN authorization_size<=16384 THEN authorization_size ELSE 0 END) OVER(ORDER BY row_key) AS authorization_used,ROW_NUMBER()OVER(ORDER BY row_key) AS ordinal FROM picked) SELECT b.space_id,b.state,b.source,CASE WHEN b.ordinal<=?4 AND b.size<=16384 AND b.used<=?3 THEN j.error_json END,COALESCE(b.size>16384 OR b.used>?3,0),b.state_limited OR octet_length(j.source_node_id)>512,CASE WHEN ?5 OR b.state<>'pending' THEN 0 WHEN b.authorization_size>16384 OR b.authorization_used>131072 THEN NULL WHEN json_valid(j.authorized_watermark) THEN COALESCE(json_extract(j.authorized_watermark,'$.authorization'),'') LIKE 'new-finalized%' ELSE NULL END FROM budgeted b JOIN embedding_job j ON j.rowid=b.row_key ORDER BY b.row_key")?;
        let mut jobs = q
            .query_map(
                params![
                    serde_json::to_string(&current_spaces).map_err(|_| serde_error())?,
                    (limits.rows + 1) as i64,
                    budget as i64,
                    limits.rows as i64,
                    self.config.indexing.auto_history
                ],
                |r| {
                    Ok(JobRow {
                        state: r.get(1)?,
                        automatic: r.get(6)?,
                        source: r.get(2)?,
                        error: r.get(3)?,
                        error_limited: r.get(4)?,
                        scalar_limited: r.get(5)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let jobs_more = jobs.len() > limits.rows;
        jobs.truncate(limits.rows);
        budget = budget.saturating_sub(
            jobs.iter()
                .filter_map(|r| r.error.as_ref())
                .map(String::len)
                .sum(),
        );
        let space_ids = spaces.iter().map(|s| &s.id).collect::<Vec<_>>();
        let mut q=db.prepare("SELECT CASE WHEN octet_length(c.space_id)<=512 THEN c.space_id ELSE '' END FROM json_each(?1) w CROSS JOIN embedding_chunk c WHERE c.space_id=w.value LIMIT ?2")?;
        let mut chunks = q
            .query_map(
                params![
                    serde_json::to_string(&space_ids).map_err(|_| serde_error())?,
                    (limits.rows + 1) as i64
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let chunks_more = chunks.len() > limits.rows;
        chunks.truncate(limits.rows);
        let mut work = Vec::new();
        let mut work_more = false;
        if args.space_id.is_none()
            && let Some(profile) = &self.profile_key
        {
            for prefix in [
                format!("index.auto.{profile}."),
                format!("index.bootstrap.{profile}."),
            ] {
                let (rows, more) = read_prefix(
                    db,
                    &prefix,
                    limits.rows.saturating_sub(work.len()),
                    &mut budget,
                )?;
                work.extend(rows);
                work_more |= more;
            }
        }
        let (probes, probes_more) = read_prefix(db, "index.probe.", 32, &mut budget)?;
        Ok(RawIndexSaved {
            args: args.clone(),
            configured: self.profile_key.is_some(),
            auto_history: self.config.indexing.auto_history,
            active,
            selected,
            spaces,
            spaces_more,
            jobs,
            jobs_more,
            chunks,
            chunks_more,
            meta: meta_rows,
            work,
            work_more,
            probes,
            probes_more,
            profile: self.profile_key.clone(),
            family: self.endpoint_family_key.clone(),
            diagnostic: self.storage_stop_reason(),
            read_at: now(),
        })
    }
}
fn count(value: u64, complete: bool) -> Value {
    json!({"value":value,"complete":complete,"relation":if complete{"exact"}else{"lower_bound"}})
}
fn vector_bytes(segments: u64, dimension: i64, complete: bool) -> Value {
    if !(1..=8192).contains(&dimension) {
        return json!({"value":null,"complete":false,"relation":"unknown","reason":"saved dimension is invalid"});
    }
    match u64::try_from(dimension)
        .ok()
        .and_then(|n| segments.checked_mul(n))
        .and_then(|n| n.checked_mul(4))
    {
        Some(value) => count(value, complete),
        None => {
            json!({"value":null,"complete":false,"relation":"unknown","reason":"saved vector size is outside the representable range"})
        }
    }
}
pub(crate) fn prepare_saved(raw: RawIndexSaved) -> Value {
    let mut counts = BTreeMap::from([
        ("pending", 0u64),
        ("held_auto_history", 0),
        ("running", 0),
        ("done", 0),
        ("failed", 0),
    ]);
    let mut unknown = 0u64;
    for row in &raw.jobs {
        let state = if !raw.auto_history && row.state == "pending" {
            match row.automatic {
                Some(true) => "held_auto_history",
                Some(false) => "pending",
                None => {
                    unknown += 1;
                    continue;
                }
            }
        } else {
            row.state.as_str()
        };
        if let Some(n) = counts.get_mut(state) {
            *n += 1;
        }
    }
    let mut reason = None;
    let mut pause_at = None;
    let mut pause_id = None;
    let mut paused = false;
    let by_key = raw
        .meta
        .iter()
        .filter_map(|r| r.body.as_ref().map(|b| (r.key.as_str(), b.as_str())))
        .collect::<BTreeMap<_, _>>();
    let rebuild_key = raw.profile.as_ref().map(|p| IndexKeys::new(p).rebuild());
    let current_group = rebuild_key
        .as_ref()
        .and_then(|key| by_key.get(key.as_str()).copied());
    let preparation=current_group.map(|group|{
        let key=IndexKeys::new(raw.profile.as_deref().unwrap()).enqueueing(group);
        let fence=by_key.get(key.as_str());
        let failed=by_key.contains_key(IndexKeys::new(raw.profile.as_deref().unwrap()).enqueue_failed(group).as_str());
        json!({"group":group,"enqueue_failed":failed,"enqueue_fence_present":fence.is_some(),"enqueue_fence_count":fence.and_then(|v|v.parse::<u64>().ok()),"note":"Admission fence is saved progress, never authorization for an automatic full-history restart; interrupted preparation requires explicit rebuild."})
    });
    let candidate_key = raw
        .profile
        .as_ref()
        .map(|p| dimensions::candidate_key(IndexKeys::new(p)));
    let candidate_record = candidate_key
        .as_ref()
        .and_then(|key| raw.meta.iter().find(|row| &row.key == key));
    let parsed_candidate = candidate_record
        .and_then(|row| row.body.as_ref())
        .and_then(|body| serde_json::from_str::<dimensions::CandidateMode>(body).ok())
        .filter(|mode| (1..=8192).contains(&mode.dimensions));
    let candidate_known =
        candidate_record.is_none_or(|row| !row.present) || parsed_candidate.is_some();
    let candidate=parsed_candidate.map(|mode|json!(mode)).or_else(||if !candidate_known{Some(json!({"available":false,"complete":false,"reason":"saved candidate type, dimensions or byte budget is unverified","full_cli":"shellboy index status"}))}else{None});
    for row in &raw.work {
        if row.scalar_limited {
            unknown += 1;
            continue;
        }
        let Some(body) = &row.body else {
            unknown += 1;
            continue;
        };
        let state = if row.key.starts_with("index.auto.") {
            decode_json::<AutoAdmission>(body).map(|w| {
                (
                    if !raw.auto_history && w.state == "pending" {
                        "held_auto_history".into()
                    } else {
                        w.state
                    },
                    w.error.map(|e| format!("Source {}: {e}", w.node_id)),
                )
            })
        } else {
            decode_json::<Work>(body).map(|w| {
                if w.group == "active" || Some(w.group.as_str()) == current_group {
                    (
                        if !raw.auto_history
                            && w.state == "pending"
                            && w.authorization.starts_with("new-finalized")
                        {
                            "held_auto_history".into()
                        } else {
                            w.state
                        },
                        w.error
                            .map(|e| format!("Source {}: {e}", w.witness.node_id)),
                    )
                } else {
                    (String::new(), None)
                }
            })
        };
        match state {
            Ok((state, error)) => {
                if let Some(n) = counts.get_mut(state.as_str()) {
                    *n += 1;
                }
                if reason.is_none() {
                    reason = error;
                }
            }
            Err(_) => unknown += 1,
        }
    }
    let pause_keys = raw
        .family
        .iter()
        .chain(raw.profile.iter())
        .map(|p| IndexKeys::new(p).pause())
        .collect::<Vec<_>>();
    for key in pause_keys {
        if let Some(body) = by_key.get(key.as_str()) {
            paused = true;
            reason = Some((*body).to_owned());
            (pause_at, pause_id) = {
                let (at, id) = pause::facts(&key, body);
                (at, Some(id))
            };
            break;
        }
    }
    if raw.args.space_id.is_none()
        && !paused
        && preparation
            .as_ref()
            .is_some_and(|p| p["enqueue_fence_present"] == true || p["enqueue_failed"] == true)
    {
        reason=Some("Rebuild source admission is in progress or was interrupted; explicitly rebuild after interrupted preparation".into());
    }
    if raw.profile.as_ref().is_some_and(|profile| {
        by_key.contains_key(IndexKeys::new(profile).recovery_required().as_str())
    }) {
        let message = queue::RECOVERY_REQUIRED_MESSAGE;
        reason = Some(match reason {
            Some(prior) => format!("{prior}; {message}"),
            None => message.into(),
        });
    }
    if reason.is_none() {
        reason = raw.jobs.iter().find_map(|j| {
            j.error
                .as_ref()
                .map(|e| format!("Source {}: {e}", j.source))
        });
    }
    if let Some(candidate) = &candidate
        && let Some(error) = candidate.get("error").filter(|v| !v.is_null())
    {
        let saved = reason.take();
        reason = Some(format!(
            "Candidate rebuild stopped at requested dimension {} ({}): {}; {}{}",
            candidate.get("dimensions").unwrap_or(&Value::Null),
            candidate
                .get("paused_at")
                .and_then(Value::as_str)
                .unwrap_or("time unknown"),
            error,
            if raw.selected.is_some() {
                "existing active index is unchanged"
            } else {
                "no active index is selected"
            },
            saved.map(|s| format!("; {s}")).unwrap_or_default()
        ));
    }
    if !raw.configured {
        reason = Some("Embedding connection/model is not configured".into());
    }
    if let Some(diagnostic) = raw.diagnostic {
        paused = true;
        reason = Some(reason.map_or(diagnostic.clone(), |s| format!("{s}; {diagnostic}")));
    }
    let metadata_limited = raw
        .meta
        .iter()
        .any(|r| r.scalar_limited || (r.present && r.body.is_none()))
        || raw.jobs.iter().any(|r| r.error_limited || r.scalar_limited)
        || raw.chunks.iter().any(String::is_empty)
        || raw.spaces.iter().any(|s| {
            s.scalar_limited
                || !(1..=8192).contains(&s.dimension)
                || s.profile.is_none()
                || s.model.is_none()
                || (s.requested_present
                    && s.requested.as_ref().is_none_or(|v| {
                        !serde_json::from_str::<usize>(v).is_ok_and(|n| (1..=8192).contains(&n))
                    }))
        })
        || !candidate_known;
    let complete = !raw.jobs_more && !raw.work_more && unknown == 0 && !metadata_limited;
    let counts = counts
        .into_iter()
        .map(|(k, v)| (k, count(v, complete)))
        .collect::<BTreeMap<_, _>>();
    let spaces=raw.spaces.iter().map(|s|{let segments=raw.chunks.iter().filter(|id|*id==&s.id).count()as u64;json!({"id":if s.scalar_limited{None}else{Some(&s.id)},"identity_known":!s.scalar_limited,"profile":s.profile,"model":s.model,"dimension":if (1..=8192).contains(&s.dimension){Some(s.dimension)}else{None},"saved_dimension":s.dimension,"dimension_known":(1..=8192).contains(&s.dimension),"state":s.state,"requested_dimensions":s.requested.as_ref().and_then(|v|serde_json::from_str::<usize>(v).ok().filter(|n|(1..=8192).contains(n))),"selected":Some(&s.id)==raw.selected.as_ref(),"segments":count(segments,!raw.chunks_more&&!s.scalar_limited),"vector_payload_bytes":vector_bytes(segments,s.dimension,!raw.chunks_more&&!s.scalar_limited),"requested_known":!s.requested_present||s.requested.as_ref().is_some_and(|v|serde_json::from_str::<usize>(v).is_ok_and(|n|(1..=8192).contains(&n))),"metadata_limited":s.scalar_limited||!(1..=8192).contains(&s.dimension)||s.profile.is_none()||s.model.is_none()||(s.requested_present&&s.requested.as_ref().is_none_or(|v|!serde_json::from_str::<usize>(v).is_ok_and(|n|(1..=8192).contains(&n))))})}).collect::<Vec<_>>();
    let mut probe_rows=raw.probes.iter().map(|row|{
        let probe=if row.scalar_limited{None}else{row.body.as_ref().and_then(|body|serde_json::from_str::<crate::protocol::IndexProbeObservation>(body).ok())};
        (&row.key,probe.map(|probe|json!(probe)).unwrap_or_else(||json!({"unavailable":"saved probe type or copy budget is unverified","complete":false,"record_key":if row.scalar_limited{None}else{Some(&row.key)},"full_cli":"shellboy index status"})))
    }).collect::<Vec<_>>();
    probe_rows.sort_by(|a, b| {
        (b.1.get("checked_at").and_then(Value::as_str), b.0)
            .cmp(&(a.1.get("checked_at").and_then(Value::as_str), a.0))
    });
    let probes = probe_rows.into_iter().map(|r| r.1).collect::<Vec<_>>();
    let probes_complete = !raw.probes_more
        && raw.probes.iter().all(|r| {
            !r.scalar_limited
                && r.body.as_ref().is_some_and(|v| {
                    serde_json::from_str::<crate::protocol::IndexProbeObservation>(v).is_ok()
                })
        });
    json!({"kind":"saved_index_observation","limits":{"saved_value_body_max_bytes":131072,"scalar_cell_bytes":SCALAR_CELL_BYTES,"scalar_channel_max_bytes":SCALAR_CHANNEL_BYTES},"read_at":raw.read_at,"scope":raw.args,"active_space":raw.active,"selected_space":raw.selected,"auto_history":raw.auto_history,"automatic_history_reason":if raw.auto_history{None}else{Some("Automatic history indexing is disabled; existing vectors and explicitly authorized index work remain available")},"status":{"counts":counts,"paused":if !metadata_limited||paused{Some(paused)}else{None},"pause_known":!metadata_limited,"reason":reason,"paused_at":pause_at,"pause_id":pause_id},"spaces":spaces,"preparation":preparation,"candidate":candidate,"candidate_known":candidate_known,"spaces_count":count(raw.spaces.len()as u64,!raw.spaces_more),"probes":probes,"probes_complete":probes_complete,"unknown_work_records":unknown,"metadata_limited":metadata_limited,"complete":complete&&!raw.spaces_more&&!raw.chunks_more&&probes_complete,"coverage":{"state":"requires_current_source_window","current":false},"continuation":{"full_cli":"shellboy index status","notice":"Counts marked lower_bound are not exact totals. Read the current source coverage window; full CLI inspection preserves the complete result."}})
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageCursor {
    version: u8,
    scope: String,
    phase: u8,
    direct: usize,
    thread: usize,
    ordinal: i64,
    after: String,
}
#[derive(Debug)]
struct SourceRow {
    id: String,
    kind: String,
    revision: i64,
    body_hash: Option<String>,
    body_hash_limited: bool,
    body: Option<String>,
    exists: bool,
    payload_present: bool,
    deleted: bool,
}
#[derive(Debug)]
struct CoverageJobRow {
    id: String,
    revision: i64,
    node: String,
    kind: String,
    hash: String,
    start: usize,
    end: usize,
    state: String,
    watermark: Option<String>,
    watermark_limited: bool,
    scalar_limited: bool,
    chunk: bool,
}
#[derive(Debug)]
struct RawRelation {
    node: String,
    execution: Option<String>,
    generation: i64,
    sequence: i64,
    encoding: String,
    state: String,
    tombstoned: bool,
    scalar_limited: bool,
}
#[derive(Debug)]
pub(crate) struct RawCoverageWindow {
    args: Arc<IndexArgs>,
    selected: Option<String>,
    read_at: String,
    next: Option<CoverageCursor>,
    inspected: usize,
    sources: Vec<SourceRow>,
    jobs: Vec<CoverageJobRow>,
    chunks: Vec<(String, String, i64, usize, usize, String)>,
    boot: Vec<MetaRow>,
    paths: Vec<(String, String, String, Option<String>)>,
    raw: Vec<RawRelation>,
    jobs_more: bool,
    paths_more: bool,
    raw_more: bool,
    all_phase: bool,
    split_chars: usize,
    rebuild_group: Option<String>,
}
/// Owned request work prepared before admission to the DB worker. In particular,
/// large explicit scopes and untrusted cursor JSON are never decoded there.
#[derive(Debug)]
pub(crate) struct PreparedCoverageRequest {
    args: Arc<IndexArgs>,
    limits: SavedReadLimits,
    direct: Vec<String>,
    threads: Vec<String>,
    threads_json: String,
    direct_window_json: String,
    base_scope: String,
    profile: Option<String>,
    position: Option<CoverageCursor>,
}
impl IndexService {
    pub(crate) fn prepare_coverage_request(
        &self,
        args: &IndexArgs,
        cursor: Option<&str>,
        limits: SavedReadLimits,
    ) -> Result<PreparedCoverageRequest, ApiError> {
        let limits = limits.checked()?;
        validate_args(args)?;
        let mut args = args.clone();
        args.source_ids.sort();
        args.source_ids.dedup();
        args.thread_ids.sort();
        args.thread_ids.dedup();
        let position = cursor
            .map(|cursor| {
                if cursor.len() > 4096 {
                    return Err(err(
                        ErrorCode::InputLimit,
                        "Coverage cursor exceeds4096 bytes",
                    ));
                }
                let value: CoverageCursor = serde_json::from_str(cursor)
                    .map_err(|_| err(ErrorCode::InvalidArgument, "Invalid coverage cursor JSON"))?;
                if value.version != 2
                    || value.phase > 3
                    || value.ordinal < -1
                    || value.scope.len() != 64
                    || !value.scope.bytes().all(|b| b.is_ascii_hexdigit())
                    || value.direct > args.source_ids.len()
                    || value.thread > args.thread_ids.len()
                {
                    return Err(err(
                        ErrorCode::InvalidArgument,
                        "Invalid coverage cursor position or scope",
                    ));
                }
                Ok(value)
            })
            .transpose()?;
        let direct = args.source_ids.clone();
        let threads = args.thread_ids.clone();
        let threads_json = serde_json::to_string(&threads).map_err(|_| db_error(serde_error()))?;
        let start = position.as_ref().map_or(0, |p| p.direct);
        let end = (start + limits.sources).min(direct.len());
        let direct_window_json =
            serde_json::to_string(&direct[start..end]).map_err(|_| db_error(serde_error()))?;
        let base_scope = hash(
            serde_json::to_vec(&(&args, &self.profile_key)).map_err(|_| db_error(serde_error()))?,
        );
        Ok(PreparedCoverageRequest {
            args: Arc::new(args),
            limits,
            direct,
            threads,
            threads_json,
            direct_window_json,
            base_scope,
            profile: self.profile_key.clone(),
            position,
        })
    }
    pub(crate) fn read_prepared_coverage_tx(
        &self,
        db: &Connection,
        request: &PreparedCoverageRequest,
    ) -> Result<RawCoverageWindow, ApiError> {
        if request.profile != self.profile_key {
            return Err(err(
                ErrorCode::InvalidArgument,
                "Coverage request belongs to another embedding profile",
            ));
        }
        self.read_coverage_inner(db, request).map_err(db_error)
    }
    fn read_coverage_inner(
        &self,
        db: &Connection,
        request: &PreparedCoverageRequest,
    ) -> Result<RawCoverageWindow, StorageError> {
        let args = &request.args;
        let limits = request.limits;
        let mut budget = limits.metadata_bytes;
        let active = read_meta(db, IndexKeys::ACTIVE_SPACE.into(), &mut budget)?;
        if active.present && active.body.is_none() {
            return Err(StorageError::from(err(
                ErrorCode::InputLimit,
                "Saved active-space identity exceeds observation budget",
            )));
        }
        let selected = args.space_id.clone().or(active.body);
        let rebuild_group = if let Some(profile) = &self.profile_key {
            let row = read_meta(db, IndexKeys::new(profile).rebuild(), &mut budget)?;
            if row.present && row.body.is_none() {
                return Err(StorageError::from(err(
                    ErrorCode::InputLimit,
                    "Saved rebuild identity exceeds observation budget",
                )));
            }
            row.body
        } else {
            None
        };
        if let Some(space) = &selected
            && read_space(db, space)?.is_none()
        {
            return Err(StorageError::from(err(
                ErrorCode::InvalidArgument,
                "Selected index space does not exist",
            )));
        }
        // Only the fixed-size request digest and one bounded saved identity
        // participate here; explicit scope serialization/hashing happened before
        // the DB admission. None and Some("") have distinct framed identities.
        if selected.as_ref().is_some_and(|id| id.len() > 4096) {
            return Err(StorageError::from(err(
                ErrorCode::InputLimit,
                "Saved selected-space identity exceeds4096 bytes",
            )));
        }
        let mut binding = request.base_scope.as_bytes().to_vec();
        binding.push(u8::from(selected.is_some()));
        if let Some(id) = &selected {
            binding.extend_from_slice(id.as_bytes());
        }
        let scope = hash(binding);
        let mut position = if let Some(cursor) = &request.position {
            if cursor.scope != scope {
                return Err(StorageError::from(err(
                    ErrorCode::InvalidArgument,
                    "Coverage scope changed; restart from its first observation window",
                )));
            }
            cursor.clone()
        } else {
            CoverageCursor {
                version: 2,
                scope,
                phase: 0,
                direct: 0,
                thread: 0,
                ordinal: -1,
                after: String::new(),
            }
        };
        let direct = &request.direct;
        let threads = &request.threads;
        let all = args.scope == Some(IndexScope::All)
            || (args.scope.is_none() && direct.is_empty() && threads.is_empty());
        let mut members = Vec::<String>::new();
        let mut all_phase = false;
        let mut direct_phase = false;
        // At most three empty scope lanes. Each nonempty lane remains bounded;
        // no global DISTINCT/sort over every Thread path is hidden in one SQL.
        while position.phase < 3 && members.is_empty() {
            match position.phase {
                0 => {
                    let end = (position.direct + limits.sources).min(direct.len());
                    members.extend_from_slice(&direct[position.direct..end]);
                    direct_phase = !members.is_empty();
                    position.direct = end;
                    if end == direct.len() {
                        position.phase = 1;
                    }
                }
                1 => {
                    if position.thread >= threads.len() {
                        position.phase = 2;
                        continue;
                    }
                    let successor = "SELECT p.ordinal FROM thread_path p WHERE p.thread_id=json_extract(?1,'$['||w.ti||']') AND p.ordinal>w.ordinal AND EXISTS(SELECT 1 FROM thread t WHERE t.id=p.thread_id AND t.deleted_at IS NULL) ORDER BY p.ordinal LIMIT 1";
                    let sql = format!(
                        "WITH RECURSIVE walk(step,ti,ordinal) AS (VALUES(0,?2,?3) UNION ALL SELECT w.step+1,CASE WHEN ({successor}) IS NULL THEN w.ti+1 ELSE w.ti END,COALESCE(({successor}),-1) FROM walk w WHERE w.step<?4 AND w.ti<?5) SELECT w.ti,w.ordinal,CASE WHEN octet_length(p.node_id)<=4096 THEN p.node_id ELSE '' END,octet_length(p.node_id)>4096 FROM walk w JOIN thread_path p ON p.thread_id=json_extract(?1,'$['||w.ti||']') AND p.ordinal=w.ordinal WHERE w.step>0 ORDER BY w.step LIMIT ?6"
                    );
                    let mut q = db.prepare(&sql)?;
                    let mut rows = q
                        .query_map(
                            params![
                                request.threads_json,
                                position.thread as i64,
                                position.ordinal,
                                (limits.sources + threads.len() + 2) as i64,
                                threads.len() as i64,
                                (limits.sources + 1) as i64
                            ],
                            |r| {
                                Ok((
                                    r.get::<_, i64>(0)?,
                                    r.get::<_, i64>(1)?,
                                    r.get::<_, String>(2)?,
                                    r.get::<_, bool>(3)?,
                                ))
                            },
                        )?
                        .collect::<Result<Vec<_>, _>>()?;
                    if rows.iter().any(|row| row.3) {
                        return Err(StorageError::from(err(
                            ErrorCode::InputLimit,
                            "Saved Thread membership identity exceeds4096 bytes; use the full CLI",
                        )));
                    }
                    let more = rows.len() > limits.sources;
                    rows.truncate(limits.sources);
                    if let Some((thread, ordinal, _, _)) = rows.last() {
                        let thread = usize::try_from(*thread).map_err(|_| {
                            StorageError::from(err(
                                ErrorCode::InvalidState,
                                "Saved coverage Thread position is outside the cursor range",
                            ))
                        })?;
                        if thread >= threads.len() || *ordinal < 0 {
                            return Err(StorageError::from(err(
                                ErrorCode::InvalidState,
                                "Saved coverage membership is outside the cursor range",
                            )));
                        }
                        position.thread = thread;
                        position.ordinal = *ordinal;
                    }
                    members.extend(rows.into_iter().map(|r| r.2));
                    if !more {
                        position.phase = 2;
                    }
                }
                2 => {
                    all_phase = true;
                    if !all {
                        position.phase = 3;
                        continue;
                    }
                    let mut q=db.prepare("SELECT CASE WHEN octet_length(id)<=4096 THEN id ELSE '' END,octet_length(id)>4096 FROM node WHERE id>?1 ORDER BY id LIMIT ?2")?;
                    let mut rows = q
                        .query_map(params![position.after, (limits.sources + 1) as i64], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    if rows.iter().any(|r| r.1) {
                        return Err(StorageError::from(err(
                            ErrorCode::InputLimit,
                            "A saved source identity exceeds coverage observation limits; use full CLI inspection",
                        )));
                    }
                    let more = rows.len() > limits.sources;
                    rows.truncate(limits.sources);
                    if let Some(last) = rows.last() {
                        position.after = last.0.clone();
                    }
                    members.extend(rows.into_iter().map(|r| r.0));
                    if !more {
                        position.phase = 3;
                    }
                }
                _ => unreachable!(),
            }
        }
        if position.phase == 1 && threads.is_empty() {
            position.phase = 2;
        }
        if position.phase == 2 && !all {
            position.phase = 3;
        }
        let inspected = members.len();
        let mut seen = HashSet::new();
        members.retain(|id| seen.insert(id.clone()));
        // Explicit-source lane JSON is prepared off-worker. Dynamic membership
        // IDs come from this transaction and are bounded to32 saved identities.
        let dynamic_ids;
        let ids = if direct_phase {
            request.direct_window_json.as_str()
        } else {
            dynamic_ids = serde_json::to_string(&members).map_err(|_| serde_error())?;
            dynamic_ids.as_str()
        };
        let mut q=db.prepare("WITH picked AS MATERIALIZED (SELECT w.key AS ordinal,w.value AS id,n.kind,n.payload_revision,n.body_hash,n.payload_json IS NOT NULL AS present,n.id IS NOT NULL AS exists_node,n.deleted_reason IS NOT NULL AS deleted,octet_length(n.payload_json) AS size FROM json_each(?1) w LEFT JOIN node n ON n.id=w.value), budgeted AS (SELECT *,SUM(CASE WHEN size<=?2 THEN size ELSE 0 END)OVER(ORDER BY ordinal)AS used FROM picked) SELECT b.id,CASE WHEN octet_length(b.kind)<=512 THEN b.kind ELSE '' END,COALESCE(b.payload_revision,0),CASE WHEN octet_length(b.body_hash)<=512 THEN b.body_hash END,CASE WHEN b.size<=?2 AND b.used<=?3 THEN n.payload_json END,b.exists_node,b.present,b.deleted,COALESCE(octet_length(b.body_hash)>512,0) FROM budgeted b LEFT JOIN node n ON n.id=b.id ORDER BY b.ordinal")?;
        let sources = q
            .query_map(
                params![ids, limits.source_bytes as i64, limits.body_bytes as i64],
                |r| {
                    Ok(SourceRow {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        revision: r.get(2)?,
                        body_hash: r.get(3)?,
                        body: r.get(4)?,
                        exists: r.get(5)?,
                        payload_present: r.get(6)?,
                        deleted: r.get(7)?,
                        body_hash_limited: r.get(8)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut q=db.prepare("WITH picked AS MATERIALIZED(SELECT CASE WHEN octet_length(a.source_node_id)<=512 THEN a.source_node_id ELSE '' END AS source_node_id,CASE WHEN octet_length(a.id)<=512 THEN a.id ELSE '' END AS id,a.rowid AS row_key,CASE WHEN octet_length(a.created_at)<=64 THEN a.created_at ELSE '' END AS at,octet_length(a.absolute_path)AS size FROM json_each(?1) w CROSS JOIN attachment_ref a INDEXED BY attachment_source WHERE a.source_node_id=w.value LIMIT 129), budgeted AS(SELECT *,SUM(CASE WHEN size<=4096 THEN size ELSE 0 END)OVER(ORDER BY source_node_id,at,id) AS used FROM picked) SELECT CASE WHEN octet_length(b.source_node_id)<=512 THEN b.source_node_id ELSE '' END,b.at,CASE WHEN octet_length(b.id)<=512 THEN b.id ELSE '' END,CASE WHEN b.size<=4096 AND b.used<=65536 THEN a.absolute_path END FROM budgeted b JOIN attachment_ref a ON a.rowid=b.row_key")?;
        let mut paths = q
            .query_map([&ids], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let paths_more = paths.len() > 128;
        paths.truncate(128);
        let mut q=db.prepare("WITH picked AS MATERIALIZED(SELECT CASE WHEN octet_length(r.node_id)<=512 THEN r.node_id ELSE '' END AS node_id,CASE WHEN octet_length(r.group_id)<=512 THEN r.group_id ELSE '' END AS group_id,CASE WHEN octet_length(r.call_id)<=512 THEN r.call_id ELSE '' END AS call_id,octet_length(r.node_id)>512 OR octet_length(r.group_id)>512 OR octet_length(r.call_id)>512 AS scalar_limited FROM json_each(?1) w CROSS JOIN tool_result r INDEXED BY tool_result_node WHERE r.node_id=w.value LIMIT 129) SELECT CASE WHEN octet_length(r.node_id)<=512 THEN r.node_id ELSE '' END,CASE WHEN octet_length(e.id)<=512 THEN e.id END,COALESCE(e.retention_generation,0),COALESCE(e.saved_sequence,0),CASE WHEN octet_length(e.output_encoding)<=64 THEN e.output_encoding ELSE '' END,CASE WHEN octet_length(e.state)<=64 THEN e.state ELSE '' END,EXISTS(SELECT 1 FROM app_meta m WHERE m.key='raw_tombstone.'||e.id),r.scalar_limited OR COALESCE(octet_length(e.id)>512 OR octet_length(e.state)>64 OR octet_length(e.output_encoding)>64,0) FROM picked r LEFT JOIN tool_call c ON c.group_id=r.group_id AND c.call_id=r.call_id LEFT JOIN shell_execution e ON e.id=c.operation_id OR e.operation_id=c.operation_id LIMIT 129")?;
        let mut raw = q
            .query_map([&ids], |r| {
                Ok(RawRelation {
                    node: r.get(0)?,
                    execution: r.get(1)?,
                    generation: r.get(2)?,
                    sequence: r.get(3)?,
                    encoding: r.get(4)?,
                    state: r.get(5)?,
                    tombstoned: r.get(6)?,
                    scalar_limited: r.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let raw_more = raw.len() > 128;
        raw.truncate(128);
        let mut jobs = Vec::new();
        let mut chunks = Vec::new();
        let mut boot = Vec::new();
        let mut jobs_more;
        if let Some(space) = &selected {
            let mut q=db.prepare("WITH picked AS MATERIALIZED(SELECT j.rowid AS row_key,CASE WHEN octet_length(j.id)<=512 THEN j.id ELSE '' END AS id,CASE WHEN octet_length(j.source_node_id)<=512 THEN j.source_node_id ELSE '' END AS source_node_id,CASE WHEN octet_length(j.body_kind)<=512 THEN j.body_kind ELSE '' END AS body_kind,CASE WHEN octet_length(j.source_hash)<=512 THEN j.source_hash ELSE '' END AS source_hash,j.span_start,j.span_end,CASE WHEN octet_length(j.state)<=64 THEN j.state ELSE '' END AS state,j.source_revision,octet_length(j.id)>512 OR octet_length(j.source_node_id)>512 OR octet_length(j.body_kind)>512 OR octet_length(j.source_hash)>512 OR octet_length(j.state)>64 AS scalar_limited,octet_length(j.authorized_watermark)AS size FROM json_each(?1) w CROSS JOIN embedding_job j WHERE j.space_id=?2 AND j.source_node_id=w.value LIMIT ?3),budgeted AS(SELECT *,SUM(CASE WHEN size<=16384 THEN size ELSE 0 END)OVER(ORDER BY id)AS used,ROW_NUMBER()OVER(ORDER BY id)AS ordinal FROM picked) SELECT CASE WHEN octet_length(b.source_node_id)<=512 THEN b.source_node_id ELSE '' END,CASE WHEN octet_length(b.body_kind)<=512 THEN b.body_kind ELSE '' END,CASE WHEN octet_length(b.source_hash)<=512 THEN b.source_hash ELSE '' END,b.span_start,b.span_end,b.state,CASE WHEN NOT b.scalar_limited AND b.ordinal<=?5 AND b.size<=16384 AND b.used<=?4 THEN j.authorized_watermark END,CASE WHEN octet_length(b.id)<=512 THEN b.id ELSE '' END,b.source_revision,COALESCE(b.size>16384 OR b.used>?4,0),b.scalar_limited FROM budgeted b JOIN embedding_job j ON j.rowid=b.row_key ORDER BY b.id")?;
            jobs = q
                .query_map(
                    params![
                        ids,
                        space,
                        (limits.rows + 1) as i64,
                        budget as i64,
                        limits.rows as i64
                    ],
                    |r| {
                        Ok(CoverageJobRow {
                            id: r.get(7)?,
                            revision: r.get(8)?,
                            node: r.get(0)?,
                            kind: r.get(1)?,
                            hash: r.get(2)?,
                            start: r.get::<_, i64>(3)? as usize,
                            end: r.get::<_, i64>(4)? as usize,
                            state: r.get(5)?,
                            watermark: r.get(6)?,
                            watermark_limited: r.get(9)?,
                            scalar_limited: r.get(10)?,
                            chunk: false,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            jobs_more = jobs.len() > limits.rows;
            jobs.truncate(limits.rows);
            let mut q=db.prepare("SELECT CASE WHEN octet_length(c.job_id)<=512 THEN c.job_id ELSE '' END,CASE WHEN octet_length(c.source_node_id)<=512 THEN c.source_node_id ELSE '' END,c.source_revision,c.span_start,c.span_end,CASE WHEN octet_length(c.content_hash)<=512 THEN c.content_hash ELSE '' END FROM json_each(?1) w CROSS JOIN embedding_chunk c INDEXED BY chunks_source WHERE c.source_node_id=w.value AND c.space_id=?2 LIMIT ?3")?;
            chunks = q
                .query_map(params![ids, space, (limits.rows + 1) as i64], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get::<_, i64>(3)? as usize,
                        r.get::<_, i64>(4)? as usize,
                        r.get(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            jobs_more |= chunks.len() > limits.rows;
            chunks.truncate(limits.rows);
        } else if let Some(profile) = &self.profile_key {
            // The source-expression index does not include profile. A join can
            // visit arbitrary old-profile rows before LIMIT; use the profile PK
            // prefix window first and filter requested source IDs off-worker.
            let prefix = format!("index.bootstrap.{profile}.");
            let (rows, more) = read_prefix(db, &prefix, limits.rows, &mut budget)?;
            boot = rows;
            jobs_more = more;
        } else {
            jobs_more = false;
        }
        let next = (position.phase < 3).then_some(position);
        Ok(RawCoverageWindow {
            args: Arc::clone(args),
            selected,
            read_at: now(),
            next,
            inspected,
            sources,
            jobs,
            chunks,
            boot,
            paths,
            raw,
            jobs_more,
            paths_more,
            raw_more,
            all_phase,
            split_chars: self.config.indexing.split_chars,
            rebuild_group,
        })
    }
}
pub(crate) fn prepare_coverage_window(mut raw: RawCoverageWindow) -> Result<Value, ApiError> {
    let next = raw.next.as_ref().map(|position| {
        let cursor = encode_json(position).map_err(db_error)?;
        if cursor.len() > 4096 {
            return Err(err(ErrorCode::InputLimit,
                "Coverage continuation exceeds4096 bytes; use the full CLI for this saved identity"));
        }
        Ok(cursor)
    }).transpose()?;
    let scalar_limited = raw.jobs.iter().any(|j| j.scalar_limited)
        || raw
            .paths
            .iter()
            .any(|p| p.0.is_empty() || p.1.is_empty() || p.2.is_empty())
        || raw.raw.iter().any(|r| r.scalar_limited)
        || raw
            .chunks
            .iter()
            .any(|c| c.0.is_empty() || c.1.is_empty() || c.5.is_empty())
        || raw.boot.iter().any(|m| m.scalar_limited);
    let mut counts = coverage::CoverageCounts::default();
    let mut unknown = Vec::new();
    let mut evaluated = 0usize;
    raw.paths
        .sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
    let mut jobs = HashMap::<String, Vec<coverage::CoverageJob>>::new();
    let mut invalid_jobs = HashSet::new();
    let chunk_keys = raw.chunks.into_iter().collect::<HashSet<_>>();
    for mut job in raw.jobs {
        job.chunk = chunk_keys.contains(&(
            job.id.clone(),
            job.node.clone(),
            job.revision,
            job.start,
            job.end,
            job.hash.clone(),
        ));
        let witness = job.watermark.as_deref().and_then(Witness::from_watermark);
        if job.watermark_limited {
            invalid_jobs.insert(job.node.clone());
        }
        jobs.entry(job.node)
            .or_default()
            .push(coverage::CoverageJob {
                kind: job.kind,
                hash: job.hash,
                start: job.start,
                end: job.end,
                state: job.state,
                witness,
                chunk: job.chunk,
            });
    }
    let wanted = raw
        .sources
        .iter()
        .map(|s| s.id.clone())
        .collect::<HashSet<_>>();
    let mut unknown_boot = false;
    for row in raw.boot {
        if row.scalar_limited {
            unknown_boot = true;
            continue;
        }
        match row.body.as_deref().map(decode_json::<Work>) {
            Some(Ok(work)) => {
                if !wanted.contains(work.witness.node_id.as_str()) {
                    continue;
                }
                if work.group != "active" && Some(&work.group) != raw.rebuild_group.as_ref() {
                    continue;
                }
                let node = work.witness.node_id.clone();
                jobs.entry(node).or_default().push(coverage::CoverageJob {
                    kind: work.body_kind,
                    hash: hash(work.segment.text.as_bytes()),
                    start: work.segment.start,
                    end: work.segment.end,
                    state: work.state,
                    witness: Some(work.witness),
                    chunk: false,
                });
            }
            _ => unknown_boot = true,
        }
    }
    for source in raw.sources {
        if raw.all_phase && !source.payload_present {
            continue;
        }
        if !source.exists || !source.payload_present || source.deleted {
            counts.unavailable_sources += 1;
            evaluated += 1;
            continue;
        }
        let limited = scalar_limited
            || source.body_hash_limited
            || raw.jobs_more
            || raw.paths_more
            || raw.raw_more
            || unknown_boot
            || invalid_jobs.contains(&source.id)
            || raw.paths.iter().any(|p| p.0 == source.id && p.3.is_none());
        let Some(body) = source.body else {
            unknown.push(json!({"source_id":source.id,"reason":"source body exceeds the finite copy budget","full_cli":"shellboy index status"}));
            continue;
        };
        let payload = match serde_json::from_str::<Value>(&body) {
            Ok(value) => value,
            Err(_) => {
                unknown.push(json!({"source_id":source.id,"reason":"malformed source JSON"}));
                continue;
            }
        };
        let relations = raw
            .raw
            .iter()
            .filter(|r| r.node == source.id && !r.tombstoned && r.execution.is_some())
            .collect::<Vec<_>>();
        if limited || relations.len() > 1 {
            unknown.push(json!({"source_id":source.id,"reason":"source hash or saved relation/job/path facts exceed budget or are unprovable"}));
            continue;
        }
        let raw_expected = !relations.is_empty();
        let witness = Witness {
            node_id: source.id.clone(),
            revision: source.revision,
            body_hash: source.body_hash,
            raw: relations
                .first()
                .filter(|r| matches!(r.state.as_str(), "exited" | "failed" | "stopped"))
                .map(|r| RawWitness {
                    execution: r.execution.clone().unwrap(),
                    generation: r.generation,
                    saved_sequence: r.sequence,
                    encoding: r.encoding.clone(),
                }),
        };
        let paths = raw
            .paths
            .iter()
            .filter(|p| p.0 == source.id)
            .filter_map(|p| p.3.clone())
            .collect::<Vec<_>>();
        let bodies = source::body_facts(&source.kind, &payload, &paths);
        let mut current = jobs.remove(&source.id).unwrap_or_default();
        current.sort_by_key(|j| (j.start, j.end));
        coverage::classify_bodies(
            &witness,
            raw_expected,
            bodies,
            current,
            raw.split_chars,
            &mut counts,
        );
        evaluated += 1;
    }
    Ok(
        json!({"kind":"index_coverage_window","limits":{"source_body_max_bytes":524288,"saved_value_body_max_bytes":131072,"scalar_cell_bytes":SCALAR_CELL_BYTES,"scalar_channel_max_bytes":SCALAR_CHANNEL_BYTES},"read_at":raw.read_at,"scope":&*raw.args,"selected_space":raw.selected,"coverage":counts,"evaluated_sources":evaluated,"inspected_memberships":raw.inspected,"unverified_sources":unknown,"window_complete":!scalar_limited&&!raw.jobs_more&&!raw.paths_more&&!raw.raw_more&&unknown.is_empty(),"scope_exhausted":raw.next.is_none(),"next_cursor":next,"consistency":"Current finite per-source observations in this window only. Different windows are not one snapshot and must not be summed as an exact current global total. IDs are deduplicated within the window; overlapping scope memberships may recur in later windows.","continuation":{"full_cli":"shellboy index status","notice":"Continue with next_cursor for unevaluated memberships. Limited source/job/path evidence remains unverified; no hidden raw decoding or provider request."}}),
    )
}

/// Shared canonical classification definitions; no duplicate UI policy.
pub(crate) fn coverage_definitions() -> Value {
    coverage::definitions()
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> (tempfile::TempDir, DbWorker, Arc<IndexService>) {
        let root = tempfile::tempdir().unwrap();
        let db = DbWorker::open(root.path().join("view.db")).await.unwrap();
        let mut config = Config::default();
        config.connections.insert(
            "fixture".into(),
            crate::config::Connection {
                base_url: "http://127.0.0.1:9/v1".into(),
                ..Default::default()
            },
        );
        config.embedding.connection = Some("fixture".into());
        config.embedding.model = Some("synthetic".into());
        config.indexing.auto_history = true;
        let service = IndexService::new(db.clone(), Arc::new(config));
        (root, db, service)
    }
    #[tokio::test]
    async fn saved_observation_preserves_disabled_automatic_history_policy() {
        let (_root, db, original) = fixture().await;
        let mut config = (*original.config).clone();
        config.indexing.auto_history = false;
        let service = IndexService::new(db.clone(), Arc::new(config));
        let observed = saved(&db, service, SavedReadLimits::default()).await;
        assert_eq!(observed["auto_history"], false);
        assert!(
            observed["automatic_history_reason"]
                .as_str()
                .unwrap()
                .contains("explicitly authorized")
        );
        assert_eq!(
            observed["status"]["paused"], false,
            "disabled automatic work is not a failure pause"
        );
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn held_automatic_jobs_are_not_executable_pending_and_unknown_is_not_exact() {
        let (_root, db, original) = fixture().await;
        seed(&db, 4).await;
        let mut config = (*original.config).clone();
        config.indexing.auto_history = false;
        let service = IndexService::new(db.clone(), Arc::new(config));
        db.call(|db| {
            db.execute("UPDATE embedding_job SET state='pending'", [])?;
            db.execute("UPDATE embedding_job SET authorized_watermark=json_set(authorized_watermark,'$.authorization','new-finalized-user') WHERE id='job-0'", [])?;
            db.execute("UPDATE embedding_job SET authorized_watermark=json_set(authorized_watermark,'$.authorization','explicit-retry') WHERE id='job-1'", [])?;
            db.execute("UPDATE embedding_job SET authorized_watermark='not-json' WHERE id='job-2'", [])?;
            db.execute("UPDATE embedding_job SET authorized_watermark=?1 WHERE id='job-3'", [json!({"authorization":"new-finalized-user","padding":"x".repeat(16384)}).to_string()])?;
            Ok(())
        }).await.unwrap();
        let observed = saved(&db, service, SavedReadLimits::default()).await;
        assert_eq!(observed["status"]["counts"]["pending"]["value"], 1);
        assert_eq!(
            observed["status"]["counts"]["held_auto_history"]["value"],
            1
        );
        assert_eq!(observed["unknown_work_records"], 2);
        assert_eq!(observed["status"]["counts"]["pending"]["complete"], false);
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn oversized_unidentifiable_work_is_deleted_and_visible_in_saved_status() {
        let (_root, db, service) = fixture().await;
        let profile = service.key().unwrap().to_owned();
        let key = IndexKeys::new(&profile).bootstrap("broken");
        db.call(move |db| {
            let body = "x".repeat(1024 * 1024 + 1);
            db.execute(
                "INSERT INTO app_meta(key,value) VALUES(?1,?2)",
                params![key, body],
            )?;
            let (changed, cursor, done) = queue::recover_inflight_page(db, None)?;
            assert!(changed && !done);
            assert_eq!(cursor.as_deref(), Some(key.as_str()));
            assert_eq!(
                db.query_row("SELECT count(*) FROM app_meta WHERE key=?1", [key], |r| r
                    .get::<_, i64>(
                    0
                ))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
        let cli = service.status(IndexArgs::default()).await.unwrap();
        assert!(
            cli.reason
                .as_deref()
                .unwrap()
                .contains("explicitly rebuild")
        );
        let observed = saved(&db, service, SavedReadLimits::default()).await;
        assert!(
            observed["status"]["reason"]
                .as_str()
                .unwrap()
                .contains("explicitly rebuild")
        );
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn recovered_source_waits_when_disabled_or_paused_then_splits() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut total = 0;
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 8192];
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(end) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
                        let length = String::from_utf8_lossy(&bytes[..end])
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            let request: Value =
                                serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                            total += request["input"].as_array().unwrap()[0]
                                .as_str()
                                .unwrap()
                                .len();
                            break;
                        }
                    }
                    assert!(bytes.len() < 2_000_000);
                }
                let body = r#"{"data":[{"index":0,"embedding":[1.0,2.0]}]}"#;
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
            }
            total
        });
        let (_root, db, original) = fixture().await;
        let mut config = (*original.config).clone();
        config.connections.get_mut("fixture").unwrap().base_url = url;
        config.indexing.split_chars = 2_000_000;
        config.indexing.auto_history = false;
        let held = IndexService::new(db.clone(), Arc::new(config.clone()));
        let profile = held.key().unwrap().to_owned();
        let text = "x".repeat(1_100_000);
        let payload =
            json!({"role":"user","content":[{"type":"input_text","text":text}]}).to_string();
        let body_hash = hash(payload.as_bytes());
        let key = IndexKeys::new(&profile).bootstrap("legacy");
        let stored_key = key.clone();
        db.call(move |db| {
            db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES('source','user','2026-01-01T00:00:00Z',?1,?2,?3)", params![payload, payload.len() as i64, body_hash])?;
            let work = Work::pending(Witness { node_id: "source".into(), revision: 0, body_hash: Some(body_hash), raw: None }, "text".into(), Segment { start: 0, end: 1_100_000, text: "x".repeat(1_100_000) }, "active", "new-finalized");
            db.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2)", params![stored_key, encode_json(&work)?])?;
            let (changed, _, done) = queue::recover_inflight_page(db, None)?;
            assert!(changed && !done);
            Ok(())
        }).await.unwrap();
        assert!(!held.drain_authorization(&profile).await.unwrap());
        let observed = saved(&db, held.clone(), SavedReadLimits::default()).await;
        assert_eq!(
            observed["status"]["counts"]["held_auto_history"]["value"],
            1
        );
        config.indexing.auto_history = true;
        let active = IndexService::new(db.clone(), Arc::new(config));
        let pause = IndexKeys::new(&profile).pause();
        let pause_copy = pause.clone();
        db.call(move |db| {
            db.execute(
                "INSERT INTO app_meta(key,value) VALUES(?1,'paused')",
                [pause_copy],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        assert!(!active.drain_authorization(&profile).await.unwrap());
        let paused_profile = profile.clone();
        db.call(move |db| {
            assert_eq!(
                claim::discard_paused(db, &paused_profile, None)?,
                Some(false)
            );
            Ok(())
        })
        .await
        .unwrap();
        db.call(move |db| {
            db.execute("DELETE FROM app_meta WHERE key=?1", [pause])?;
            Ok(())
        })
        .await
        .unwrap();
        assert!(active.drain_authorization(&profile).await.unwrap());
        let paused_profile = profile.clone();
        db.call(move |db| {
            let pause = IndexKeys::new(&paused_profile).pause();
            db.execute(
                "INSERT INTO app_meta(key,value) VALUES(?1,'paused')",
                [&pause],
            )?;
            assert_eq!(
                claim::discard_paused(db, &paused_profile, None)?,
                Some(false)
            );
            db.execute("DELETE FROM app_meta WHERE key=?1", [&pause])?;
            Ok(())
        })
        .await
        .unwrap();
        db.call(move |db| {
            let mut q = db.prepare("SELECT value FROM app_meta WHERE key LIKE ?1 ORDER BY key")?;
            let rows = q
                .query_map([IndexKeys::new(&profile).bootstrap_pattern()], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            assert!(rows.len() >= 2, "expected split bootstrap units");
            let mut parts = rows
                .iter()
                .map(|row| {
                    assert!(row.len() <= 1024 * 1024);
                    decode_json::<Work>(row).unwrap().segment
                })
                .collect::<Vec<_>>();
            parts.sort_by_key(|part| part.start);
            assert_eq!(
                parts.iter().map(|part| part.text.len()).sum::<usize>(),
                1_100_000
            );
            assert_eq!(parts.first().unwrap().start, 0);
            assert_eq!(parts.last().unwrap().end, 1_100_000);
            Ok(())
        })
        .await
        .unwrap();
        for _ in 0..5 {
            let progress = crate::test_watchdog::watchdog(
                "recovered source processing",
                active.process_one(CancellationToken::new()),
            )
            .await
            .unwrap()
            .unwrap();
            if !progress {
                break;
            }
        }
        assert_eq!(
            crate::test_watchdog::watchdog("recovered source provider", server)
                .await
                .unwrap()
                .unwrap(),
            1_100_000
        );
        let done = db
            .call(|db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM embedding_chunk WHERE source_node_id='source'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(done, 2);
        db.shutdown().await.unwrap();
    }
    async fn seed(db: &DbWorker, count: usize) {
        db.call(move|db|{
            let tx=db.transaction()?;
            tx.execute("INSERT INTO index_space(id,profile_id,model_id,dimension,splitter_version,state,vec_table_name)VALUES('space','fixture','synthetic',2,'fixture','active','shellboy_vec_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')",[])?;
            tx.execute("INSERT INTO app_meta(key,value)VALUES('index.active_space','space')",[])?;
            for i in 0..count{
                let id=format!("source-{i:04}");let payload=json!({"role":"user","content":[{"type":"input_text","text":"abc"}]}).to_string();let body_hash=hash(payload.as_bytes());
                tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,'user','2026-01-01T00:00:00Z',?2,?3,?4)",params![id,payload,payload.len()as i64,body_hash])?;
                let bodies=history::search_bodies("user",&serde_json::from_str::<Value>(&payload).unwrap());let kind=&bodies[0].0;
                let job=format!("job-{i}");let watermark=json!({"witness":{"node_id":id,"revision":0,"body_hash":body_hash,"raw":null}}).to_string();
                tx.execute("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,authorized_watermark)VALUES(?1,'space',?2,0,?3,?4,0,3,'done',?5)",params![job,id,kind,hash(b"abc"),watermark])?;
                tx.execute("INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id)VALUES(?1,?2,'space',?3,0,0,3,?4,?5)",params![format!("chunk-{i}"),job,id,hash(b"abc"),i as i64])?;
            }tx.commit()?;Ok(())
        }).await.unwrap();
    }
    async fn saved(db: &DbWorker, service: Arc<IndexService>, limits: SavedReadLimits) -> Value {
        let raw = db
            .call(move |db| Ok(service.read_saved_tx(db, &IndexArgs::default(), limits)))
            .await
            .unwrap()
            .unwrap();
        prepare_saved(raw)
    }
    fn coverage(raw: RawCoverageWindow) -> Value {
        prepare_coverage_window(raw).unwrap()
    }
    fn continuation(raw: &RawCoverageWindow) -> Option<String> {
        raw.next.as_ref().map(|cursor| encode_json(cursor).unwrap())
    }
    async fn window(
        db: &DbWorker,
        service: Arc<IndexService>,
        args: IndexArgs,
        cursor: Option<String>,
        limits: SavedReadLimits,
    ) -> RawCoverageWindow {
        let request = service
            .prepare_coverage_request(&args, cursor.as_deref(), limits)
            .unwrap();
        db.call(move |db| Ok(service.read_prepared_coverage_tx(db, &request)))
            .await
            .unwrap()
            .unwrap()
    }
    #[tokio::test]
    async fn finite_normal_saved_counts_and_current_canonical_coverage_match_existing_exact_cli() {
        let (_root, db, service) = fixture().await;
        seed(&db, 2).await;
        let old = service
            .management_status(IndexArgs::default())
            .await
            .unwrap();
        let now = saved(&db, service.clone(), SavedReadLimits::default()).await;
        assert_eq!(
            now["status"]["counts"]["done"]["value"],
            old["status"]["done"]
        );
        assert_eq!(now["status"]["counts"]["done"]["complete"], true);
        let raw = window(
            &db,
            service.clone(),
            IndexArgs::default(),
            None,
            SavedReadLimits::default(),
        )
        .await;
        let current = coverage(raw);
        assert_eq!(current["coverage"], old["coverage"]);
        assert_eq!(current["window_complete"], true);
        assert_eq!(current["scope_exhausted"], true);
        db.call(|db|{db.execute("UPDATE embedding_job SET authorized_watermark=json_set(authorized_watermark,'$.witness.revision',1)",[])?;Ok(())}).await.unwrap();
        let old = service
            .management_status(IndexArgs::default())
            .await
            .unwrap();
        let current = coverage(
            window(
                &db,
                service,
                IndexArgs::default(),
                None,
                SavedReadLimits::default(),
            )
            .await,
        );
        assert_eq!(current["coverage"], old["coverage"]);
        assert_eq!(current["coverage"]["stale_segments"], 2);
        assert_eq!(current["coverage"]["fully_indexed_sources"], 0);
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn source_and_job_budgets_never_report_unread_evidence_as_full_or_zero_exact() {
        let (_root, db, service) = fixture().await;
        seed(&db, 3).await;
        let limits = SavedReadLimits {
            rows: 1,
            ..Default::default()
        };
        let current = saved(&db, service.clone(), limits).await;
        assert_eq!(current["status"]["counts"]["done"]["value"], 1);
        assert_eq!(
            current["status"]["counts"]["done"]["relation"],
            "lower_bound"
        );
        let raw = window(&db, service.clone(), IndexArgs::default(), None, limits).await;
        let current = coverage(raw);
        assert_eq!(current["coverage"]["fully_indexed_sources"], 0);
        assert_eq!(current["unverified_sources"].as_array().unwrap().len(), 3);
        assert_eq!(current["window_complete"], false);
        db.call(|db|{let body=json!({"role":"user","content":[{"type":"input_text","text":"x".repeat(128*1024)}]}).to_string();db.execute("UPDATE node SET payload_json=?1,body_hash=?2 WHERE id='source-0000'",params![body,hash(body.as_bytes())])?;Ok(())}).await.unwrap();
        let raw = window(
            &db,
            service,
            IndexArgs {
                source_ids: vec!["source-0000".into()],
                ..Default::default()
            },
            None,
            SavedReadLimits::default(),
        )
        .await;
        assert!(raw.sources[0].body.is_none());
        let current = coverage(raw);
        assert_eq!(current["window_complete"], false);
        assert_eq!(current["coverage"]["fully_indexed_sources"], 0);
        assert_eq!(current["unverified_sources"].as_array().unwrap().len(), 1);
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn sparse_thread_membership_cursor_advances_without_history_scan_or_false_snapshot() {
        let (_root, db, service) = fixture().await;
        seed(&db, 3).await;
        db.call(|db|{db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('live','/','','fixture','synthetic','now'),('empty','/','','fixture','synthetic','now')",[])?;for(i,ordinal)in[0,10,100].into_iter().enumerate(){db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('live',?1,?2)",params![ordinal,format!("source-{i:04}")])?;}Ok(())}).await.unwrap();
        let args = IndexArgs {
            thread_ids: vec!["empty".into(), "live".into()],
            ..Default::default()
        };
        let limits = SavedReadLimits {
            sources: 2,
            ..Default::default()
        };
        let first = window(&db, service.clone(), args.clone(), None, limits).await;
        assert_eq!(first.sources.len(), 2);
        let cursor = continuation(&first).unwrap();
        assert_eq!(coverage(first)["scope_exhausted"], false);
        let second = window(&db, service, args, Some(cursor), limits).await;
        assert_eq!(second.sources.len(), 1);
        assert_eq!(second.sources[0].id, "source-0002");
        assert_eq!(coverage(second)["scope_exhausted"], true);
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn saved_pause_and_probe_oversize_is_explicit_and_simple_reads_authorize_nothing() {
        let (_root, db, service) = fixture().await;
        seed(&db, 1).await;
        let pause = IndexKeys::new(service.profile_key.as_deref().unwrap()).pause();
        db.call(move |db| {
            db.execute(
                "INSERT INTO app_meta(key,value)VALUES(?1,?2),('index.probe.large',?2)",
                params![pause, "x".repeat(20000)],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let value = saved(&db, service.clone(), SavedReadLimits::default()).await;
        assert_eq!(value["metadata_limited"], true);
        assert_eq!(value["status"]["pause_known"], false);
        assert_eq!(value["probes_complete"], false);
        db.call(|db|{assert_eq!(db.query_row("SELECT COUNT(*) FROM app_meta WHERE key LIKE 'index.auto.%' OR key LIKE 'index.bootstrap.%'",[],|r|r.get::<_,i64>(0))?,0);Ok(())}).await.unwrap();
        assert!(!service.started.load(Ordering::Acquire));
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn oversized_nonnull_source_hash_cannot_become_null_witness_proof() {
        let (_root, db, service) = fixture().await;
        seed(&db, 1).await;
        db.call(|db|{db.execute("UPDATE node SET body_hash=?1 WHERE id='source-0000'",["h".repeat(513)])?;db.execute("UPDATE embedding_job SET authorized_watermark=json_set(authorized_watermark,'$.witness.body_hash',NULL)",[])?;Ok(())}).await.unwrap();
        let raw = window(
            &db,
            service,
            IndexArgs::default(),
            None,
            SavedReadLimits::default(),
        )
        .await;
        assert!(raw.sources[0].body_hash_limited);
        assert!(raw.sources[0].body_hash.is_none());
        let current = coverage(raw);
        assert_eq!(current["coverage"]["fully_indexed_sources"], 0);
        assert_eq!(current["unverified_sources"].as_array().unwrap().len(), 1);
        assert_eq!(current["window_complete"], false);
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn typed_invalid_candidate_probe_and_requested_dimension_are_explicitly_unknown() {
        let (_root, db, service) = fixture().await;
        seed(&db, 1).await;
        let candidate =
            dimensions::candidate_key(IndexKeys::new(service.profile_key.as_deref().unwrap()));
        db.call(move|db|{db.execute("INSERT INTO app_meta(key,value)VALUES(?1,'{\"group\":\"g\",\"dimensions\":0}'),('index.probe.invalid','{\"checked_at\":\"now\"}'),('index.space_dimensions.space','0')",[candidate])?;Ok(())}).await.unwrap();
        let value = saved(&db, service, SavedReadLimits::default()).await;
        assert_eq!(value["candidate_known"], false);
        assert_eq!(value["candidate"]["complete"], false);
        assert_eq!(value["probes_complete"], false);
        assert_eq!(value["probes"][0]["complete"], false);
        assert_eq!(value["spaces"][0]["requested_known"], false);
        assert!(value["spaces"][0]["requested_dimensions"].is_null());
        assert_eq!(value["complete"], false);
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn emitted_cursor_is_accepted_or_identity_returns_explicit_limit_error() {
        let (_root, db, service) = fixture().await;
        db.call(|db|{db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}'),('z','user','now','{}')",["a".repeat(4096)])?;Ok(())}).await.unwrap();
        let reader = service.clone();
        let limits = SavedReadLimits {
            sources: 1,
            ..Default::default()
        };
        let request = reader
            .prepare_coverage_request(&IndexArgs::default(), None, limits)
            .unwrap();
        let raw = db
            .call(move |db| Ok(reader.read_prepared_coverage_tx(db, &request)))
            .await
            .unwrap()
            .unwrap();
        let error = prepare_coverage_window(raw).unwrap_err();
        assert_eq!(error.code, ErrorCode::InputLimit);
        db.call(|db|{db.execute("DELETE FROM node",[])?;db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}'),('z','user','now','{}')",["a".repeat(512)])?;Ok(())}).await.unwrap();
        let first = window(&db, service.clone(), IndexArgs::default(), None, limits).await;
        let cursor = continuation(&first).unwrap();
        assert!(cursor.len() <= 4096);
        let second = window(
            &db,
            service.clone(),
            IndexArgs::default(),
            Some(cursor),
            limits,
        )
        .await;
        assert_eq!(second.sources[0].id, "z");
        db.call(|db|{db.execute("DELETE FROM node",[])?;db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}'),('z','user','now','{}')",["a\0b"])?;Ok(())}).await.unwrap();
        let first = window(&db, service.clone(), IndexArgs::default(), None, limits).await;
        assert_eq!(first.sources[0].id, "a\0b");
        let cursor = continuation(&first);
        let second = window(&db, service, IndexArgs::default(), cursor, limits).await;
        assert_eq!(second.sources[0].id, "z");
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn scalar_copy_guard_marks_oversized_saved_job_label_unverified() {
        let (_root, db, service) = fixture().await;
        seed(&db, 1).await;
        db.call(|db| {
            db.execute("UPDATE embedding_job SET body_kind=?1", ["k".repeat(10000)])?;
            Ok(())
        })
        .await
        .unwrap();
        let raw = window(
            &db,
            service,
            IndexArgs::default(),
            None,
            SavedReadLimits::default(),
        )
        .await;
        assert!(raw.jobs[0].scalar_limited);
        assert!(raw.jobs[0].kind.is_empty());
        let value = coverage(raw);
        assert_eq!(value["window_complete"], false);
        assert_eq!(value["coverage"]["fully_indexed_sources"], 0);
        assert_eq!(value["limits"]["scalar_cell_bytes"], 512);
        assert_eq!(value["unverified_sources"].as_array().unwrap().len(), 1);
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn coverage_preparation_rejects_bad_inputs_without_a_running_database() {
        let (_root, db, service) = fixture().await;
        db.shutdown().await.unwrap();
        // Even the valid request succeeds with the DB worker already shut down:
        // neither validation nor cursor/scoped-input preparation admits DB work.
        assert!(
            service
                .prepare_coverage_request(&IndexArgs::default(), None, SavedReadLimits::default())
                .is_ok()
        );
        let invalid = service
            .prepare_coverage_request(&IndexArgs::default(), Some("{"), SavedReadLimits::default())
            .unwrap_err();
        assert_eq!(invalid.code, ErrorCode::InvalidArgument);
        let cursor = "x".repeat(4097);
        assert_eq!(
            service
                .prepare_coverage_request(
                    &IndexArgs::default(),
                    Some(&cursor),
                    SavedReadLimits::default()
                )
                .unwrap_err()
                .code,
            ErrorCode::InputLimit
        );
        for args in [
            IndexArgs {
                source_ids: vec!["source".into(); 129],
                ..Default::default()
            },
            IndexArgs {
                thread_ids: vec!["t".repeat(4097)],
                ..Default::default()
            },
            IndexArgs {
                space_id: Some("s".repeat(4097)),
                ..Default::default()
            },
        ] {
            assert_eq!(
                service
                    .prepare_coverage_request(&args, None, SavedReadLimits::default())
                    .unwrap_err()
                    .code,
                ErrorCode::InputLimit
            );
        }
        let invalid = json!({"version":2,"scope":"0".repeat(64),"phase":0,"direct":1,"thread":0,"ordinal":-1,"after":""}).to_string();
        assert_eq!(
            service
                .prepare_coverage_request(
                    &IndexArgs::default(),
                    Some(&invalid),
                    SavedReadLimits::default()
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }

    #[tokio::test]
    async fn owned_prepared_scope_normalizes_ids_and_emitted_cursor_reads_the_next_window() {
        let (_root, db, service) = fixture().await;
        seed(&db, 3).await;
        let limits = SavedReadLimits {
            sources: 1,
            ..Default::default()
        };
        let args = IndexArgs {
            source_ids: vec![
                "source-0002".into(),
                "source-0000".into(),
                "source-0000".into(),
            ],
            ..Default::default()
        };
        let request = service
            .prepare_coverage_request(&args, None, limits)
            .unwrap();
        drop(args);
        assert_eq!(request.direct, vec!["source-0000", "source-0002"]);
        assert_eq!(request.direct_window_json, "[\"source-0000\"]");
        let reader = service.clone();
        let raw = db
            .call(move |db| Ok(reader.read_prepared_coverage_tx(db, &request)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(raw.sources[0].id, "source-0000");
        let value = prepare_coverage_window(raw).unwrap();
        let args: IndexArgs = serde_json::from_value(value["scope"].clone()).unwrap();
        let cursor = value["next_cursor"].as_str().unwrap().to_owned();
        let request = service
            .prepare_coverage_request(&args, Some(&cursor), limits)
            .unwrap();
        drop(args);
        drop(cursor);
        let reader = service.clone();
        let raw = db
            .call(move |db| Ok(reader.read_prepared_coverage_tx(db, &request)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(raw.sources[0].id, "source-0002");
        assert!(prepare_coverage_window(raw).unwrap()["next_cursor"].is_null());
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn prepared_cursor_rejects_selected_space_change_without_silent_rebinding() {
        let (_root, db, service) = fixture().await;
        seed(&db, 2).await;
        let limits = SavedReadLimits {
            sources: 1,
            ..Default::default()
        };
        let first = window(&db, service.clone(), IndexArgs::default(), None, limits).await;
        let value = prepare_coverage_window(first).unwrap();
        let cursor = value["next_cursor"].as_str().unwrap();
        let request = service
            .prepare_coverage_request(&IndexArgs::default(), Some(cursor), limits)
            .unwrap();
        db.call(|db|{
            db.execute("INSERT INTO index_space(id,profile_id,model_id,dimension,splitter_version,state,vec_table_name)VALUES('other','fixture','synthetic',2,'fixture','active','shellboy_vec_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb')",[])?;
            db.execute("UPDATE app_meta SET value='other' WHERE key='index.active_space'",[])?;
            Ok(())
        }).await.unwrap();
        let reader = service.clone();
        let result = db
            .call(move |db| Ok(reader.read_prepared_coverage_tx(db, &request)))
            .await
            .unwrap();
        let error = result.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains("scope changed"));
        // Only explicit first-window preparation adopts the new saved space.
        let fresh = window(&db, service, IndexArgs::default(), None, limits).await;
        assert_eq!(fresh.selected.as_deref(), Some("other"));
        db.shutdown().await.unwrap();
    }
}
