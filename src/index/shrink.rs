//! Explicit, API-free conversion of the active space to int8 (#253).
//!
//! The conversion reuses the space lifecycle: an int8 `candidate` is filled from
//! the active float32 space, which stays searchable until the candidate is
//! complete, and stays afterwards too: nothing here removes a completed space.
//! One durable record (`index.shrink.<source space>`) carries the
//! progress; every unit commits with it, so an interruption resumes at the cursor.
//! The existing index workers drive it through the background lane.
use super::*;
use crate::protocol::{IndexShrinkArgs, IndexShrinkStatus};

/// Vectors read or converted per background unit. Measured on 3,072 dimensions;
/// see docs/design/search-and-relations.md.
const UNIT: std::num::NonZeroU32 = std::num::NonZeroU32::new(32).unwrap();
/// Symmetric: -128 would give one side a longer range than the other.
const INT8_LIMIT: f64 = 127.0;
const SCALE: &str = "scale";
const COPY: &str = "copy";
const SWEEP: &str = "sweep";
const REVERT: &str = "revert";
const REQUEUE: &str = "requeue";
/// A chunk of space ?1 at row ?2 whose exact segment space ?3 has not indexed.
const MISSING_ROW: &str = "SELECT c.source_node_id FROM embedding_chunk c JOIN embedding_job j ON j.id=c.job_id WHERE c.space_id=?1 AND c.vec_row_id=?2 AND NOT EXISTS(SELECT 1 FROM embedding_job f WHERE f.space_id=?3 AND f.source_node_id=j.source_node_id AND f.source_revision=j.source_revision AND f.body_kind=j.body_kind AND f.span_start=j.span_start AND f.span_end=j.span_end AND f.source_hash=j.source_hash AND f.state='done')";

/// Saturating; a space's scale is fixed, so a later larger value clips to ±127.
pub(super) fn quantize(vector: &[f32], scale: f64) -> Vec<i8> {
    vector
        .iter()
        .map(|value| {
            (f64::from(*value) * scale)
                .round()
                .clamp(-INT8_LIMIT, INT8_LIMIT) as i8
        })
        .collect()
}

/// Durable conversion state. At most one exists: starting refuses otherwise.
/// Removed on completion, explicit cancel, or when the source stops being active.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Record {
    source: String,
    target: Option<String>,
    phase: String,
    /// Last `vec_row_id` handled by the current phase.
    cursor: i64,
    /// Largest absolute element seen by the scale phase.
    max_abs: f64,
    started_at: String,
    error: Option<Value>,
}

fn read_record(db: &Connection) -> Result<Option<Record>, StorageError> {
    let body: Option<String> = db
        .query_row(
            "SELECT value FROM app_meta WHERE key>=?1 AND key<?2 ORDER BY key LIMIT 1",
            [IndexKeys::SHRINK_START, IndexKeys::SHRINK_END],
            |row| row.get(0),
        )
        .optional()?;
    body.as_deref().map(decode_json).transpose()
}

/// The lowest row ID a new vector of `space` may take while it is being
/// converted: rows at or below the cursor are never read again.
pub(super) fn first_unread_row(db: &Connection, space: &str) -> Result<i64, StorageError> {
    Ok(json_meta::<Record>(db, &IndexKeys::shrink(space))?.map_or(0, |record| record.cursor + 1))
}

pub(super) fn involves(db: &Connection, space: &str) -> Result<bool, StorageError> {
    Ok(read_record(db)?
        .is_some_and(|record| record.source == space || record.target.as_deref() == Some(space)))
}

pub(super) fn status(db: &Connection) -> Result<Option<IndexShrinkStatus>, StorageError> {
    let Some(record) = read_record(db)? else {
        return Ok(None);
    };
    let segments = |space: &str| {
        db.query_row(
            "SELECT count(*) FROM embedding_chunk WHERE space_id=?1",
            [space],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count as u64)
    };
    Ok(Some(IndexShrinkStatus {
        source_segments: segments(&record.source)?,
        target_segments: record.target.as_deref().map_or(Ok(0), segments)?,
        source_space: record.source,
        target_space: record.target,
        phase: record.phase,
        started_at: record.started_at,
        error: record.error,
    }))
}

enum Step {
    /// Nothing can happen until a wake (a running claim settling).
    Wait,
    Progress(Record),
    /// The record ends; the old space may be collected.
    Finished,
}
enum Outcome {
    Idle,
    Progress,
    Finished,
}

impl IndexService {
    /// One bounded unit, `true` when the worker should come back immediately.
    pub(super) async fn shrink_unit(&self) -> Result<bool, ApiError> {
        let config = self.config.clone();
        let Some(outcome) = self
            .background_unit(UNIT, move |db, limit| unit(db, &config, limit))
            .await?
        else {
            return Ok(true); // Deferred: nothing committed; retry next round.
        };
        Ok(match outcome {
            Outcome::Idle => false,
            Outcome::Progress => {
                self.publish_saved(true);
                true
            }
            Outcome::Finished => {
                self.publish_saved(true);
                self.spaces_changed();
                self.wake();
                true
            }
        })
    }

    /// Preview, start or resume, or cancel. Never calls an embedding API.
    pub async fn shrink(&self, args: IndexShrinkArgs) -> Result<Value, ApiError> {
        crate::protocol::Command::IndexShrink(args.clone()).validate()?;
        let config = self.config.clone();
        if args.dry_run {
            return self
                .database
                .call(move |db| {
                    Ok(if args.revert {
                        revert_preview(db, &config)?.0
                    } else {
                        preview(db, &config)?.0
                    })
                })
                .await
                .map_err(db_error);
        }
        let result = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let value = if args.cancel {
                    cancel(&tx)?
                } else if args.revert {
                    start_revert(&tx, &config)?
                } else {
                    start(&tx, &config)?
                };
                tx.commit()?;
                Ok(value)
            })
            .await
            .map_err(db_error)?;
        self.publish_saved(true);
        if args.cancel {
            self.spaces_changed();
        }
        self.wake();
        Ok(result)
    }
}

fn unit(
    db: &mut Connection,
    config: &Config,
    limit: crate::storage::Limit,
) -> Result<Outcome, StorageError> {
    let tx = db.transaction()?;
    let Some(record) = read_record(&tx)? else {
        return Ok(Outcome::Idle);
    };
    let key = IndexKeys::shrink(&record.source);
    // Before the failure filter: a later explicit choice also releases a failed run.
    if matches!(record.phase.as_str(), SCALE | COPY | REVERT)
        && active_source(&tx, &record)?.is_none()
    {
        drop_candidate(&tx, &record)?;
        tx.execute("DELETE FROM app_meta WHERE key=?1", [&key])?;
        tx.commit()?;
        return Ok(Outcome::Finished);
    }
    if record.error.is_some() {
        return Ok(Outcome::Idle);
    }
    match step(&tx, config, record.clone(), limit) {
        Ok(Step::Wait) => Ok(Outcome::Idle),
        Ok(Step::Progress(next)) => {
            set_json_meta(&tx, &key, &next)?;
            tx.commit()?;
            Ok(Outcome::Progress)
        }
        Ok(Step::Finished) => {
            tx.execute("DELETE FROM app_meta WHERE key=?1", [&key])?;
            tx.commit()?;
            Ok(Outcome::Finished)
        }
        Err(error) => {
            // Keep the old space and the last committed progress; the failure
            // waits for an explicit `index shrink` to resume or `--cancel`.
            drop(tx);
            let error = error.into_api_error();
            let mut failed = record;
            failed.error = Some(json!({"code":error.code,"message":error.message,"at":now()}));
            let at_cap = error.code == ErrorCode::StorageLimit;
            let tx = db.transaction()?;
            if at_cap {
                // A partial candidate must not keep the owner next to the cap, so
                // give its bytes back at once; a later run starts over.
                drop_candidate(&tx, &failed)?;
                failed.target = None;
                failed.phase = SCALE.into();
                failed.cursor = 0;
                failed.max_abs = 0.0;
            }
            set_json_meta(&tx, &key, &failed)?;
            tx.commit()?;
            Ok(if at_cap {
                Outcome::Finished
            } else {
                Outcome::Progress
            })
        }
    }
}

fn active_source(db: &Connection, record: &Record) -> Result<Option<Space>, StorageError> {
    let active = meta(db, IndexKeys::ACTIVE_SPACE)?;
    Ok(load_space(db, &record.source)?.filter(|space| {
        active.as_deref() == Some(space.id.as_str())
            && matches!(space.state.as_str(), "active" | "paused")
    }))
}

fn step(
    tx: &Transaction<'_>,
    config: &Config,
    mut record: Record,
    limit: crate::storage::Limit,
) -> Result<Step, StorageError> {
    if record.phase == SWEEP {
        return sweep_page(tx, record, limit);
    }
    if record.phase == REQUEUE {
        return requeue_page(tx, record, limit);
    }
    let source = active_source(tx, &record)?.ok_or_else(serde_error)?;
    if record.phase == REVERT {
        let target = record.target.as_deref().ok_or_else(serde_error)?;
        if source_running(tx, &source.id)? {
            return Ok(Step::Wait);
        }
        switch(tx, &source, target)?;
        // Pending admissions would block a later rebuild and spend later on their own.
        let automatic = requeue_policy(tx, config, target)? == "automatic";
        record.phase = if automatic { REQUEUE } else { SWEEP }.into();
        record.cursor = 0;
        return Ok(Step::Progress(record));
    }
    let rows = chunk_rows(tx, &source.id, record.cursor, limit)?;
    match record.phase.as_str() {
        SCALE => {
            let mut vectors = VectorReader::new(tx, &source)?;
            for row in &rows {
                for value in vectors.read(*row)? {
                    record.max_abs = record.max_abs.max(f64::from(value.abs()));
                }
            }
            drop(vectors);
            if let Some(last) = rows.last() {
                record.cursor = *last;
            } else {
                if record.max_abs <= 0.0 {
                    return Err(StorageError::from(err(
                        ErrorCode::InvalidState,
                        "The active index has no non-zero vector to derive a scale from",
                    )));
                }
                let scale = INT8_LIMIT / record.max_abs;
                record.target = Some(create_candidate(tx, &source, scale)?);
                record.phase = COPY.into();
                record.cursor = 0;
            }
            Ok(Step::Progress(record))
        }
        COPY => {
            let target = record
                .target
                .as_deref()
                .and_then(|id| load_space(tx, id).transpose())
                .transpose()?
                .filter(|space| space.state == "candidate" && space.int8_scale.is_some())
                .ok_or_else(serde_error)?;
            if rows.is_empty() {
                // No vector is ahead of the cursor. A claim in flight may add one.
                if source_running(tx, &source.id)? {
                    return Ok(Step::Wait);
                }
                switch(tx, &source, &target.id)?;
                set_meta(tx, &IndexKeys::converted_from(&target.id), &source.id)?;
                record.phase = SWEEP.into();
                record.cursor = 0;
                return Ok(Step::Progress(record));
            }
            let mut vectors = VectorReader::new(tx, &source)?;
            for row in &rows {
                copy_row(tx, &target, &source.id, *row, &mut vectors)?;
                record.cursor = *row;
            }
            // After the copy: this sees the unit's uncommitted bytes. The old space
            // stays afterwards, so optional growth must leave conversations room
            // below the cap that holds new work.
            crate::gc::require_capacity_below_tx(tx, room_limit(config)).map_err(|_| {
                StorageError::from(err(
                    ErrorCode::StorageLimit,
                    "The conversion stopped before reaching the logical data warning level; it paused nothing and held no work",
                ))
            })?;
            // The volume too: a conversion adds bytes, so it stops at the free-space
            // minimum like new work does, and the unit rolls back with it.
            crate::gc::require_disk_free(config, crate::gc::database_free_bytes(tx), 0).map_err(
                |_| {
                    StorageError::from(err(
                        ErrorCode::StorageLimit,
                        "The conversion stopped because free disk space is below limits.disk_free_min_bytes; it paused nothing and held no work. Free space and run `index shrink` again",
                    ))
                },
            )?;
            Ok(Step::Progress(record))
        }
        _ => Err(serde_error()),
    }
}

/// Existing configured level, no new number: optional maintenance stops here.
fn room_limit(config: &Config) -> u64 {
    config
        .limits
        .data_warning_bytes
        .min(config.limits.data_max_bytes)
}

fn source_running(tx: &Transaction<'_>, space: &str) -> Result<bool, StorageError> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM embedding_job WHERE space_id=?1 AND state='running')",
        [space],
        |row| row.get(0),
    )?)
}

/// The one atomic switch. The old space stays, inactive, with all its vectors.
fn switch(tx: &Transaction<'_>, source: &Space, target: &str) -> Result<(), StorageError> {
    tx.execute(
        "UPDATE index_space SET state='inactive' WHERE state IN ('active','paused')",
        [],
    )?;
    let moved = tx.execute(
        "UPDATE index_space SET state=?2 WHERE id=?1",
        params![target, source.state],
    )?;
    // Never leave the active pointer dangling; the whole unit rolls back.
    if moved != 1 {
        return Err(StorageError::from(err(
            ErrorCode::InvalidState,
            "The index to switch to no longer exists",
        )));
    }
    set_meta(tx, IndexKeys::ACTIVE_SPACE, target)
}

/// Whether a revert may hand missing sources to automatic admission now.
fn requeue_policy(
    db: &Connection,
    config: &Config,
    target: &str,
) -> Result<&'static str, StorageError> {
    if !config.indexing.auto_history {
        return Ok("auto_history_off");
    }
    let splitter = load_space(db, target)?.ok_or_else(serde_error)?.splitter;
    let Some(key) =
        identity::profile_key(config).filter(|key| splitter == IndexKeys::new(key).splitter())
    else {
        return Ok("other_configuration");
    };
    let family = identity::endpoint_family_key(config);
    for scope in std::iter::once(key.as_str()).chain(family.as_deref()) {
        if meta(db, &IndexKeys::new(scope).pause())?.is_some() {
            return Ok("paused");
        }
    }
    Ok("automatic")
}

/// After a revert: sources indexed only in the int8 space go through the existing
/// automatic admission, entered only when it is on and not paused. Existing jobs of
/// the float space dedupe every segment that is already there.
fn requeue_page(
    tx: &Transaction<'_>,
    mut record: Record,
    limit: crate::storage::Limit,
) -> Result<Step, StorageError> {
    let target_id = record.target.as_deref().ok_or_else(serde_error)?;
    let target = load_space(tx, target_id)?.ok_or_else(serde_error)?;
    let key = target
        .splitter
        .strip_prefix(&format!("{SPLITTER}:"))
        .ok_or_else(serde_error)?
        .to_owned();
    let rows = chunk_rows(tx, &record.source, record.cursor, limit)?;
    let Some(last) = rows.last() else {
        record.phase = SWEEP.into();
        record.cursor = 0;
        return Ok(Step::Progress(record));
    };
    record.cursor = *last;
    let mut missing = tx.prepare_cached(MISSING_ROW)?;
    for row in rows {
        let node: Option<String> = missing
            .query_row(params![record.source, row, target_id], |r| r.get(0))
            .optional()?;
        if let Some(node_id) = node {
            let admission = AutoAdmission {
                recovery: false,
                recovery_group: None,
                recovery_authorization: None,
                recovery_witness: None,
                recovery_body_kind: None,
                node_id: node_id.clone(),
                state: "pending".into(),
                error: None,
            };
            tx.execute(
                "INSERT OR IGNORE INTO app_meta(key,value)VALUES(?1,?2)",
                params![
                    IndexKeys::new(&key).auto(&node_id),
                    encode_json(&admission)?
                ],
            )?;
        }
    }
    drop(missing);
    Ok(Step::Progress(record))
}

fn chunk_rows(
    tx: &Transaction<'_>,
    space: &str,
    after: i64,
    limit: crate::storage::Limit,
) -> Result<Vec<i64>, StorageError> {
    let mut rows = tx.prepare_cached(
        "SELECT vec_row_id FROM embedding_chunk WHERE space_id=?1 AND vec_row_id>?2 ORDER BY vec_row_id LIMIT ?3",
    )?;
    rows.query_map(params![space, after, limit.get()], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()
        .map_err(StorageError::from)
}

struct VectorReader<'a> {
    statement: rusqlite::Statement<'a>,
    dimension: usize,
}
impl<'a> VectorReader<'a> {
    fn new(tx: &'a Transaction<'_>, space: &Space) -> Result<Self, StorageError> {
        valid_table(&space.table)?;
        Ok(Self {
            statement: tx.prepare(&format!(
                "SELECT embedding FROM {} WHERE rowid=?1",
                space.table
            ))?,
            dimension: space.dimension,
        })
    }
    /// A float32 vector, rejected rather than repaired if it is not intact.
    fn read(&mut self, row: i64) -> Result<Vec<f32>, StorageError> {
        let bytes: Vec<u8> = self
            .statement
            .query_row([row], |row| row.get(0))
            .optional()?
            .ok_or_else(|| {
                StorageError::from(err(
                    ErrorCode::InvalidState,
                    format!("Vector row {row} is missing from the active index"),
                ))
            })?;
        let vector: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        if bytes.len() != self.dimension * 4 || vector.iter().any(|v| !v.is_finite()) {
            return Err(StorageError::from(err(
                ErrorCode::InvalidState,
                format!("Vector row {row} of the active index is malformed"),
            )));
        }
        Ok(vector)
    }
}

fn create_candidate(
    tx: &Transaction<'_>,
    source: &Space,
    scale: f64,
) -> Result<String, StorageError> {
    let space = id();
    let table = format!("shellboy_vec_{}", uuid::Uuid::new_v4().simple());
    valid_table(&table)?;
    tx.execute_batch(&format!(
        "CREATE VIRTUAL TABLE {table} USING vec0(embedding int8[{}]);",
        source.dimension
    ))?;
    tx.execute(
        "INSERT INTO index_space(id,profile_id,model_id,dimension,splitter_version,state,vec_table_name,int8_scale)VALUES(?1,?2,?3,?4,?5,'candidate',?6,?7)",
        params![space, source.profile, source.model, source.dimension as i64, source.splitter, table, scale],
    )?;
    // Same endpoint/model request identity: queries stay compatible.
    if let Some(marker) = meta(tx, &IndexKeys::query_profile(&source.id))? {
        identity::bind_query_profile(tx, &space, &marker)?;
    }
    dimensions::bind_space(tx, &space, source.requested_dimensions)?;
    Ok(space)
}

/// One vector with its job, chunk and preview; the job's source witness is kept,
/// so search validates the copy exactly as it validates the original.
fn copy_row(
    tx: &Transaction<'_>,
    target: &Space,
    source: &str,
    row: i64,
    vectors: &mut VectorReader<'_>,
) -> Result<(), StorageError> {
    type Copied = (String, Option<String>, Option<String>, Option<String>);
    let found: Option<Copied> = tx
        .prepare_cached(
            "SELECT c.id,c.job_id,j.state,m.value FROM embedding_chunk c LEFT JOIN embedding_job j ON j.id=c.job_id LEFT JOIN app_meta m ON m.key=?3||c.id WHERE c.space_id=?1 AND c.vec_row_id=?2",
        )?
        .query_row(params![source, row, IndexKeys::PREVIEW_PREFIX], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let Some((chunk, Some(job), Some(state), preview)) = found else {
        return Ok(());
    };
    // Search ignores a chunk whose job is not done; it needs no copy.
    if state != "done" {
        return Ok(());
    }
    let encoded = target
        .encode_vector(&vectors.read(row)?)
        .map_err(|_| serde_error())?;
    tx.execute(
        &format!(
            "INSERT INTO {}(rowid,embedding)VALUES(?1,{})",
            target.table,
            target.vector_sql("?2")
        ),
        params![row, encoded],
    )?;
    let (new_job, new_chunk) = (id(), id());
    tx.prepare_cached(
        "INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,attempts,authorized_watermark) SELECT ?1,?2,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,attempts,authorized_watermark FROM embedding_job WHERE id=?3",
    )?
    .execute(params![new_job, target.id, job])?;
    tx.prepare_cached(
        "INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id) SELECT ?1,?2,?3,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id FROM embedding_chunk WHERE id=?4",
    )?
    .execute(params![new_chunk, new_job, target.id, chunk])?;
    if let Some(preview) = preview {
        set_meta(tx, &IndexKeys::preview(&new_chunk), &preview)?;
    }
    Ok(())
}

/// After the switch the old space's unfinished jobs would be stranded: inactive
/// spaces are never claimed. Move them without calling any API; they keep their
/// saved input, schedule and failure, and a later embedding is stored as int8.
fn sweep_page(
    tx: &Transaction<'_>,
    record: Record,
    limit: crate::storage::Limit,
) -> Result<Step, StorageError> {
    let target = record.target.as_deref().ok_or_else(serde_error)?;
    let jobs: Vec<String> = tx
        .prepare_cached(
            "SELECT id FROM embedding_job INDEXED BY jobs_ready WHERE space_id=?1 AND state IN ('pending','failed','split') LIMIT ?2",
        )?
        .query_map(params![record.source, limit.get()], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if jobs.is_empty() {
        return Ok(Step::Finished);
    }
    for job in jobs {
        let moved = tx.execute(
            "UPDATE OR IGNORE embedding_job SET space_id=?2 WHERE id=?1",
            params![job, target],
        )?;
        if moved == 0 {
            // The new space already queued this exact segment.
            tx.execute(
                "DELETE FROM app_meta WHERE key=?1",
                [IndexKeys::input(&job)],
            )?;
            tx.execute("DELETE FROM embedding_job WHERE id=?1", [job])?;
        }
    }
    Ok(Step::Progress(record))
}

fn drop_candidate(tx: &Transaction<'_>, record: &Record) -> Result<(), StorageError> {
    if let Some(target) = &record.target {
        tx.execute(
            "UPDATE index_space SET state='discarded' WHERE id=?1 AND state='candidate'",
            [target],
        )?;
    }
    Ok(())
}

/// The facts a person consents to; `state` says whether it can start. Read-only.
fn preview(db: &Connection, config: &Config) -> Result<(Value, &'static str), StorageError> {
    let record = read_record(db)?;
    let active = meta(db, IndexKeys::ACTIVE_SPACE)?
        .map(|id| load_space(db, &id))
        .transpose()?
        .flatten();
    let Some(space) = active else {
        return Ok((
            json!({"dry_run":true,"state":"no_active_index","embedding_requests":0}),
            "no_active_index",
        ));
    };
    let segments = count_chunks(db, &space.id)?;
    let unfinished: i64 = db.query_row(
        "SELECT count(*) FROM embedding_job WHERE space_id=?1 AND state IN ('pending','failed')",
        [&space.id],
        |row| row.get(0),
    )?;
    let bytes = segments as u64 * space.dimension as u64;
    let logical = crate::gc::logical_total_tx(db).map_err(StorageError::from)?;
    let state = match &record {
        Some(record) if record.error.is_some() => "failed",
        Some(_) => "running",
        None if space.int8_scale.is_some() => "already_int8",
        None if segments == 0 => "empty",
        None if logical + bytes >= room_limit(config) => "low_room",
        None if low_disk(db, config, bytes).is_some() => "low_disk",
        None => "ready",
    };
    let retained = retained_source(db, &space)?;
    let value = json!({
        "dry_run":true,
        "state":state,
        "space_id":space.id,
        "connection":space.profile,
        "model":space.model,
        "dimension":space.dimension,
        "int8_scale":space.int8_scale,
        "segments":segments,
        "vector_payload_bytes":bytes * space.element_bytes(),
        "vector_payload_bytes_after":bytes,
        "added_vector_payload_bytes":bytes,
        "logical_bytes":logical,
        "logical_bytes_after":logical + bytes,
        "logical_warning_bytes":config.limits.data_warning_bytes,
        "logical_limit_bytes":config.limits.data_max_bytes,
        "unfinished_jobs_moved":unfinished,
        "embedding_requests":0,
        "retained_source":retained,
        "low_disk":(state == "low_disk").then(|| low_disk_message(db, config, bytes)),
        "filesystem_free_bytes":crate::gc::database_free_bytes(db),
        "low_room":(state == "low_room").then(|| low_room_message(logical, logical + bytes, config)),
        "conversion":record.map(|record| json!({"phase":record.phase,"error":record.error})),
        "effects":"Adds an int8 copy of this index without any API call and switches search to it, which then reads about a quarter of the vector bytes. It deletes nothing: the old index stays, inactive, counts toward the logical data limit and frees no disk until a person removes it with `index remove`. `index shrink --revert` switches back. Rank order changes slightly and distance values change scale.",
    });
    Ok((value, state))
}

/// `Some(free)` when the added bytes would leave less than the free-space minimum.
fn low_disk(db: &Connection, config: &Config, adding: u64) -> Option<u64> {
    let free = crate::gc::database_free_bytes(db);
    crate::gc::require_disk_free(config, free, adding)
        .is_err()
        .then_some(free.unwrap_or(0))
}

fn low_disk_message(db: &Connection, config: &Config, adding: u64) -> String {
    format!(
        "Converting adds about {adding} bytes and the volume has {} bytes free; that would leave less than limits.disk_free_min_bytes ({}), so nothing was changed. Free space on the data directory's volume, or lower limits.disk_free_min_bytes if you accept less margin. Removing a retained index (`index remove`) frees space for reuse inside the database file and only `storage vacuum` shrinks the file; that is a person's decision",
        crate::gc::database_free_bytes(db).unwrap_or(0),
        config.limits.disk_free_min_bytes
    )
}

fn low_room_message(logical: u64, after: u64, config: &Config) -> String {
    format!(
        "Converting would take logical data from {logical} to {after} bytes, reaching the limit of {} bytes for optional growth, so nothing was changed. Free data, or raise `limits.data_warning_bytes` in the configuration if you accept less margin. Any retained indexes in `index status` are an option for the user to remove",
        room_limit(config)
    )
}

fn count_chunks(db: &Connection, space: &str) -> Result<i64, StorageError> {
    Ok(db.query_row(
        "SELECT count(*) FROM embedding_chunk WHERE space_id=?1",
        [space],
        |row| row.get(0),
    )?)
}

fn retained_source(db: &Connection, active: &Space) -> Result<Option<Value>, StorageError> {
    let Some(source) = meta(db, &IndexKeys::converted_from(&active.id))?
        .map(|id| load_space(db, &id))
        .transpose()?
        .flatten()
        .filter(|space| space.state == "inactive" && space.int8_scale.is_none())
    else {
        return Ok(None);
    };
    let segments = count_chunks(db, &source.id)? as u64;
    Ok(Some(json!({
        "space_id":source.id,
        "vector_payload_bytes":segments * source.dimension as u64 * source.element_bytes(),
        "remove_with":format!("shellboy index remove --space {} --dry-run", source.id),
    })))
}

fn revert_preview(db: &Connection, config: &Config) -> Result<(Value, &'static str), StorageError> {
    let active = meta(db, IndexKeys::ACTIVE_SPACE)?
        .map(|id| load_space(db, &id))
        .transpose()?
        .flatten();
    let target = match &active {
        Some(space) if space.int8_scale.is_some() => retained_source(db, space)?,
        _ => None,
    };
    let state = if read_record(db)?.is_some() {
        "running"
    } else if target.is_some() {
        "ready"
    } else {
        "no_retained_source"
    };
    let (mut missing_segments, mut missing_sources) = (0i64, 0i64);
    if let (Some(space), Some(target)) = (&active, &target) {
        // One read over the active space's chunks, as `index status` already does.
        (missing_segments, missing_sources) = db.query_row(
            "SELECT count(*),count(DISTINCT c.source_node_id) FROM embedding_chunk c JOIN embedding_job j ON j.id=c.job_id WHERE c.space_id=?1 AND NOT EXISTS(SELECT 1 FROM embedding_job f WHERE f.space_id=?2 AND f.source_node_id=j.source_node_id AND f.source_revision=j.source_revision AND f.body_kind=j.body_kind AND f.span_start=j.span_start AND f.span_end=j.span_end AND f.source_hash=j.source_hash AND f.state='done')",
            params![space.id, target["space_id"].as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    }
    let requeue = match target.as_ref().and_then(|t| t["space_id"].as_str()) {
        Some(id) => requeue_policy(db, config, id)?,
        None => "none",
    };
    let value = json!({
        "dry_run":true,
        "revert":true,
        "requeue":requeue,
        "state":state,
        "active_space":active.as_ref().map(|space| &space.id),
        "target":target,
        "missing_segments":missing_segments,
        "missing_sources":missing_sources,
        "embedding_requests":0,
        "effects":"Switches search back to the retained float32 index. Segments indexed since the conversion are not in it. With `requeue` = automatic, their sources go back through normal automatic indexing, which charges for them, and only kinds in `indexing.auto_kinds` are taken. Otherwise (automatic indexing off, paused, or another configuration) nothing is queued: find them with `index preview` and add them with `index backfill`, which charges for them. The int8 index stays, inactive, until a person removes it.",
    });
    Ok((value, state))
}

fn start_revert(tx: &Transaction<'_>, config: &Config) -> Result<Value, StorageError> {
    let (preview, state) = revert_preview(tx, config)?;
    let invalid = |message: &str| Err(StorageError::from(err(ErrorCode::InvalidState, message)));
    match state {
        "ready" => {
            let source = preview["active_space"].as_str().ok_or_else(serde_error)?;
            let target = preview["target"]["space_id"]
                .as_str()
                .ok_or_else(serde_error)?;
            set_json_meta(
                tx,
                &IndexKeys::shrink(source),
                &Record {
                    source: source.to_owned(),
                    target: Some(target.to_owned()),
                    phase: REVERT.into(),
                    cursor: 0,
                    max_abs: 0.0,
                    started_at: now(),
                    error: None,
                },
            )?;
            Ok(json!({
                "accepted":true,
                "state":"reverting",
                "target_space":target,
                "missing_segments":preview["missing_segments"],
                "missing_sources":preview["missing_sources"],
                "requeue":preview["requeue"],
            }))
        }
        "running" => invalid("A conversion is running; let it finish or cancel it first"),
        _ => invalid("The active index has no retained float32 index to switch back to"),
    }
}

fn start(tx: &Transaction<'_>, config: &Config) -> Result<Value, StorageError> {
    let (preview, state) = preview(tx, config)?;
    let invalid = |message: &str| Err(StorageError::from(err(ErrorCode::InvalidState, message)));
    match state {
        "ready" => {
            let source = preview["space_id"].as_str().ok_or_else(serde_error)?;
            set_json_meta(
                tx,
                &IndexKeys::shrink(source),
                &Record {
                    source: source.to_owned(),
                    target: None,
                    phase: SCALE.into(),
                    cursor: 0,
                    max_abs: 0.0,
                    started_at: now(),
                    error: None,
                },
            )?;
            Ok(json!({
                "accepted":true,
                "state":"started",
                "source_space":source,
                "embedding_requests":0,
                "source_after":"retained until removed with index remove",
                "source_vector_payload_bytes":preview["vector_payload_bytes"],
            }))
        }
        "failed" => {
            let mut record = read_record(tx)?.ok_or_else(serde_error)?;
            if matches!(record.phase.as_str(), SCALE | COPY) {
                // Only what is still to be copied; after the limit that is everything.
                let copied = match &record.target {
                    Some(target) => count_chunks(tx, target)? as u64,
                    None => 0,
                };
                let dimension = preview["dimension"].as_u64().ok_or_else(serde_error)?;
                let segments = preview["segments"].as_u64().ok_or_else(serde_error)?;
                let logical = preview["logical_bytes"].as_u64().ok_or_else(serde_error)?;
                let after = logical + segments.saturating_sub(copied) * dimension;
                if after >= room_limit(config) {
                    return invalid(&low_room_message(logical, after, config));
                }
                let added = segments.saturating_sub(copied) * dimension;
                if low_disk(tx, config, added).is_some() {
                    return invalid(&low_disk_message(tx, config, added));
                }
            }
            record.error = None;
            set_json_meta(tx, &IndexKeys::shrink(&record.source), &record)?;
            Ok(
                json!({"accepted":true,"state":"resumed","source_space":record.source,"embedding_requests":0}),
            )
        }
        "running" => Ok(json!({"accepted":true,"state":"running","embedding_requests":0})),
        "already_int8" => invalid("The active index is already int8"),
        "empty" => invalid("The active index has no vectors to convert"),
        "low_room" => invalid(preview["low_room"].as_str().unwrap_or("Not enough room")),
        "low_disk" => invalid(
            preview["low_disk"]
                .as_str()
                .unwrap_or("Not enough free disk space"),
        ),
        _ => invalid("There is no active index to convert"),
    }
}

fn cancel(tx: &Transaction<'_>) -> Result<Value, StorageError> {
    let invalid = |message: &str| StorageError::from(err(ErrorCode::InvalidState, message));
    let record = read_record(tx)?.ok_or_else(|| invalid("No index conversion is running"))?;
    if matches!(record.phase.as_str(), SWEEP | REQUEUE) {
        return Err(invalid(
            "The switch has already happened; only its unfinished work is still moving",
        ));
    }
    drop_candidate(tx, &record)?;
    tx.execute(
        "DELETE FROM app_meta WHERE key=?1",
        [IndexKeys::shrink(&record.source)],
    )?;
    Ok(json!({"cancelled":true,"source_space":record.source}))
}

#[cfg(test)]
#[path = "shrink_tests.rs"]
mod tests;
