//! Explicit, API-free conversion of the active space to int8 (#253).
//!
//! The conversion reuses the space lifecycle: an int8 `candidate` is filled from
//! the active float32 space, which stays searchable until the candidate is
//! complete. One durable record (`index.shrink.<source space>`) carries the
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
        if args.dry_run {
            return self
                .database
                .call(|db| Ok(preview(db)?.0))
                .await
                .map_err(db_error);
        }
        let result = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let value = if args.cancel {
                    cancel(&tx)?
                } else {
                    start(&tx)?
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
    if record.phase != SWEEP && active_source(&tx, &record)?.is_none() {
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
    let source = active_source(tx, &record)?.ok_or_else(serde_error)?;
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
                let running: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM embedding_job WHERE space_id=?1 AND state='running')",
                    [&source.id],
                    |row| row.get(0),
                )?;
                if running {
                    return Ok(Step::Wait);
                }
                tx.execute(
                    "UPDATE index_space SET state='inactive' WHERE state IN ('active','paused')",
                    [],
                )?;
                tx.execute(
                    "UPDATE index_space SET state=?2 WHERE id=?1",
                    params![target.id, source.state],
                )?;
                set_meta(tx, IndexKeys::ACTIVE_SPACE, &target.id)?;
                record.phase = SWEEP.into();
                record.cursor = 0;
                return Ok(Step::Progress(record));
            }
            let mut vectors = VectorReader::new(tx, &source)?;
            for row in &rows {
                copy_row(tx, &target, &source.id, *row, &mut vectors)?;
                record.cursor = *row;
            }
            // After the copy: this sees the unit's uncommitted bytes. A committed
            // unit must never take the owner to the cap, which holds new work.
            crate::gc::require_capacity_tx(tx, config).map_err(StorageError::from)?;
            Ok(Step::Progress(record))
        }
        _ => Err(serde_error()),
    }
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
            "UPDATE index_space SET state='inactive' WHERE id=?1 AND state='candidate'",
            [target],
        )?;
    }
    Ok(())
}

/// The facts a person consents to; `state` says whether it can start. Read-only.
fn preview(db: &Connection) -> Result<(Value, &'static str), StorageError> {
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
    let segments: i64 = db.query_row(
        "SELECT count(*) FROM embedding_chunk WHERE space_id=?1",
        [&space.id],
        |row| row.get(0),
    )?;
    let unfinished: i64 = db.query_row(
        "SELECT count(*) FROM embedding_job WHERE space_id=?1 AND state IN ('pending','failed')",
        [&space.id],
        |row| row.get(0),
    )?;
    let bytes = segments as u64 * space.dimension as u64;
    let state = match &record {
        Some(record) if record.error.is_some() => "failed",
        Some(_) => "running",
        None if space.int8_scale.is_some() => "already_int8",
        None if segments == 0 => "empty",
        None => "ready",
    };
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
        "temporary_extra_vector_payload_bytes":bytes,
        "unfinished_jobs_moved":unfinished,
        "embedding_requests":0,
        "conversion":record.map(|record| json!({"phase":record.phase,"error":record.error})),
        "effects":"Converts this index in place of the old one without any API call; a search then reads about a quarter of the vector bytes. The old index keeps serving search until the new one is complete, then it is removed by storage collection, so going back to full precision requires a paid rebuild. Rank order changes slightly and distance values change scale. The database file does not shrink by itself; freed pages are reused or need storage vacuum.",
    });
    Ok((value, state))
}

fn start(tx: &Transaction<'_>) -> Result<Value, StorageError> {
    let (preview, state) = preview(tx)?;
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
            Ok(
                json!({"accepted":true,"state":"started","source_space":source,"embedding_requests":0}),
            )
        }
        "failed" => {
            let mut record = read_record(tx)?.ok_or_else(serde_error)?;
            record.error = None;
            set_json_meta(tx, &IndexKeys::shrink(&record.source), &record)?;
            Ok(
                json!({"accepted":true,"state":"resumed","source_space":record.source,"embedding_requests":0}),
            )
        }
        "running" => Ok(json!({"accepted":true,"state":"running","embedding_requests":0})),
        "already_int8" => invalid("The active index is already int8"),
        "empty" => invalid("The active index has no vectors to convert"),
        _ => invalid("There is no active index to convert"),
    }
}

fn cancel(tx: &Transaction<'_>) -> Result<Value, StorageError> {
    let invalid = |message: &str| StorageError::from(err(ErrorCode::InvalidState, message));
    let record = read_record(tx)?.ok_or_else(|| invalid("No index conversion is running"))?;
    if record.phase == SWEEP {
        return Err(invalid(
            "The converted index is already active; only its unfinished jobs are still moving",
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
