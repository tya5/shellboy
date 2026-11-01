//! Finite UI storage observations. Exact CLI accounting/collection is unchanged.
//! Saved rows belong to the caller's transaction; physical samples never do.
use super::{ApiError, BlockingFiles, ErrorCode};
use crate::config::Retention;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub rows_per_index: usize,
    pub executions: usize,
    pub output_witnesses: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            rows_per_index: 1024,
            executions: 128,
            output_witnesses: 1024,
        }
    }
}
impl Limits {
    fn valid(self) -> Result<Self, ApiError> {
        if self.rows_per_index > 1024 || self.executions > 128 || self.output_witnesses > 1024 {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "Storage observation limits exceed the finite read budget",
            ));
        }
        Ok(self)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Category {
    History,
    Raw,
    Index,
    Managed,
}
struct Prefix {
    category: Category,
    bytes: Vec<u64>,
    complete: bool,
    examined: usize,
}
struct Execution {
    terminal: bool,
    pinned: bool,
    marked: bool,
    ended: Option<String>,
    started: Option<String>,
    bytes: Vec<u64>,
    complete: bool,
}
pub(crate) struct RawStorageObservation {
    prefixes: Vec<Prefix>,
    executions: Vec<Execution>,
    executions_complete: bool,
    raw_examined: usize,
    reusable: u64,
    at: DateTime<Utc>,
    cutoff: Option<DateTime<Utc>>,
    quota: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct BytesObservation {
    pub bytes: u64,
    pub complete: bool,
    pub examined_rows: usize,
    pub limited_reason: Option<&'static str>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct StorageObservation {
    pub observed_at: String,
    pub history: BytesObservation,
    pub raw: BytesObservation,
    pub index: BytesObservation,
    pub managed: BytesObservation,
    pub logical: BytesObservation,
    pub reusable_bytes: u64,
    pub reclaimable: BytesObservation,
}
// Match exact CLI byte expressions/partial predicates. Index order is scalar+rowid,
// not an ID sort over the full table; no payload/JSON is transferred to the UI.
const SCALARS: [(&str, &str, &str, &str, Category); 13] = [
    (
        "node",
        "gc_bytes_node",
        "octet_length(payload_json)",
        "1",
        Category::History,
    ),
    (
        "output_item",
        "gc_bytes_output_item",
        "octet_length(payload_json)",
        "1",
        Category::History,
    ),
    (
        "instruction_snapshot",
        "gc_bytes_instruction_snapshot",
        "octet_length(body)",
        "1",
        Category::History,
    ),
    (
        "draft",
        "gc_bytes_draft",
        "octet_length(body_json)",
        "1",
        Category::History,
    ),
    (
        "model_attempt",
        "gc_bytes_model_attempt",
        "COALESCE(octet_length(input_manifest_json),0)+COALESCE(octet_length(error_json),0)+COALESCE(octet_length(usage_json),0)",
        "1",
        Category::History,
    ),
    (
        "event",
        "gc_bytes_event",
        "octet_length(payload_ref_json)+octet_length(source_json)",
        "1",
        Category::History,
    ),
    (
        "receipt",
        "gc_bytes_receipt",
        "octet_length(sender_json)+COALESCE(octet_length(result_json),0)",
        "1",
        Category::History,
    ),
    (
        "operation_receipt",
        "gc_bytes_operation_receipt",
        "octet_length(origin_json)+COALESCE(octet_length(result_ref_json),0)",
        "1",
        Category::History,
    ),
    (
        "tool_call",
        "gc_bytes_tool_call",
        "octet_length(arguments_json)",
        "1",
        Category::History,
    ),
    (
        "app_meta",
        "gc_bytes_app_meta_history",
        "octet_length(value)",
        "key LIKE 'summary.part.%' OR key LIKE 'wait.pending.%' OR key LIKE 'compact.result.%' OR key LIKE 'revisit.%' OR key LIKE 'summary.usage.%'",
        Category::History,
    ),
    (
        "output_chunk",
        "gc_bytes_output_chunk",
        "length(bytes)",
        "1",
        Category::Raw,
    ),
    (
        "managed_file",
        "gc_bytes_managed_file",
        "raw_size",
        "state<>'deleted'",
        Category::Managed,
    ),
    (
        "app_meta",
        "gc_bytes_app_meta_index",
        "octet_length(value)",
        "key LIKE 'index.%'",
        Category::Index,
    ),
];
fn scalar_sql(table: &str, index: &str, expression: &str, predicate: &str) -> String {
    format!(
        "SELECT {expression} FROM {table} INDEXED BY {index} WHERE {predicate} ORDER BY {expression},rowid LIMIT ?1"
    )
}
fn sql_error(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("bounded storage observation: {error}"))
}
fn nonnegative(value: Option<i64>) -> Result<u64, ApiError> {
    match value {
        None => Ok(0),
        Some(value) => u64::try_from(value)
            .map_err(|_| crate::storage::save_error("negative saved byte count")),
    }
}
/// No transaction/worker is started here. Call inside the shared read transaction.
pub(crate) fn read_tx(
    db: &Connection,
    limits: Limits,
    retention: &Retention,
    at: DateTime<Utc>,
) -> Result<RawStorageObservation, ApiError> {
    let limits = limits.valid()?;
    let mut prefixes = Vec::with_capacity(14);
    for (table, index, expression, predicate, category) in SCALARS {
        let mut statement = db
            .prepare(&scalar_sql(table, index, expression, predicate))
            .map_err(sql_error)?;
        let values = statement
            .query_map([(limits.rows_per_index + 1) as i64], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?;
        let examined = values.len();
        let complete = examined <= limits.rows_per_index;
        let bytes = values
            .into_iter()
            .take(limits.rows_per_index)
            .map(nonnegative)
            .collect::<Result<Vec<_>, _>>()?;
        prefixes.push(Prefix {
            category,
            bytes,
            complete,
            examined,
        });
    }
    let mut statement=db.prepare("SELECT (SELECT dimension FROM index_space s WHERE s.id=c.space_id) FROM embedding_chunk c ORDER BY c.rowid LIMIT ?1").map_err(sql_error)?;
    let dimensions = statement
        .query_map([(limits.rows_per_index + 1) as i64], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    let examined = dimensions.len();
    let complete = examined <= limits.rows_per_index && dimensions.iter().all(Option::is_some);
    let bytes = dimensions
        .into_iter()
        .take(limits.rows_per_index)
        .map(|dimension| {
            nonnegative(dimension).and_then(|n| {
                n.checked_mul(4)
                    .ok_or_else(|| crate::storage::save_error("vector byte count overflow"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    prefixes.push(Prefix {
        category: Category::Index,
        bytes,
        complete,
        examined,
    });
    let mut statement=db.prepare("SELECT CASE WHEN octet_length(id)<=256 THEN id END,state,CASE WHEN octet_length(ended_at)<=128 THEN ended_at END,CASE WHEN octet_length(started_at)<=128 THEN started_at END,EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind='shell_output' AND resource_id=s.id),EXISTS(SELECT 1 FROM app_meta WHERE key='raw_tombstone.'||s.id) FROM shell_execution s ORDER BY s.rowid LIMIT ?1").map_err(sql_error)?;
    let rows = statement
        .query_map([(limits.executions + 1) as i64], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, bool>(5)?,
            ))
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    let samples = output_samples(
        db,
        &rows
            .iter()
            .take(limits.executions)
            .map(|row| row.0.as_deref())
            .collect::<Vec<_>>(),
        limits.output_witnesses,
    )?;
    let mut executions_complete = rows.len() <= limits.executions;
    let mut executions = Vec::new();
    let mut remaining = limits.output_witnesses;
    let raw_examined = samples.iter().map(Vec::len).sum();
    for ((id, state, ended, started, pinned, marked), sizes) in
        rows.into_iter().take(limits.executions).zip(samples)
    {
        let Some(_) = id else {
            executions_complete = false;
            continue;
        };
        if remaining == 0 {
            executions_complete = false;
            break;
        }
        let complete = sizes.len() <= remaining;
        let used = sizes.len().min(remaining);
        let bytes = sizes
            .into_iter()
            .take(remaining)
            .map(nonnegative)
            .collect::<Result<Vec<_>, _>>()?;
        remaining -= used;
        executions.push(Execution {
            terminal: super::terminal(&state),
            pinned,
            marked,
            ended,
            started,
            bytes,
            complete,
        });
        // One combined sentinel; no per-execution refill beyond the budget.
        if !complete {
            executions_complete = false;
            break;
        }
    }
    let free = db
        .query_row("PRAGMA freelist_count", [], |row| row.get::<_, i64>(0))
        .map_err(sql_error)?;
    let page = db
        .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))
        .map_err(sql_error)?;
    Ok(RawStorageObservation {
        prefixes,
        executions,
        executions_complete,
        raw_examined,
        reusable: nonnegative(Some(free))?
            .checked_mul(nonnegative(Some(page))?)
            .ok_or_else(|| crate::storage::save_error("reusable byte count overflow"))?,
        at,
        cutoff: super::cutoff(at, retention.tool_raw_days),
        quota: retention.tool_raw_bytes,
    })
}
fn output_samples(
    db: &Connection,
    ids: &[Option<&str>],
    budget: usize,
) -> Result<Vec<Vec<Option<i64>>>, ApiError> {
    let mut out = vec![Vec::new(); ids.len()];
    if budget == 0 || ids.is_empty() {
        return Ok(out);
    }
    let owners = serde_json::to_string(ids).expect("bounded UTF8 execution identifiers serialize");
    // Each step either emits the next indexed chunk or advances past one empty
    // owner. The walk and its final sort are bounded before eligibility filters.
    // CASE makes the first seek include every valid sequence, including gaps.
    let sql="WITH RECURSIVE owners AS MATERIALIZED (
 SELECT CAST(key AS INTEGER) ordinal,value id FROM json_each(?1)
), walk(step,owner,sequence,size,witnesses,has_chunk) AS (
 SELECT 0,0,NULL,NULL,0,0
 UNION ALL
 SELECT w.step+1,CASE WHEN c.rowid IS NULL THEN w.owner+1 ELSE w.owner END,
 c.sequence,length(c.bytes),w.witnesses+(c.rowid IS NOT NULL),c.rowid IS NOT NULL
 FROM walk w CROSS JOIN owners o ON o.ordinal=w.owner
 LEFT JOIN output_chunk c ON c.rowid=CASE WHEN w.sequence IS NULL THEN
  (SELECT rowid FROM output_chunk WHERE execution_id=o.id ORDER BY sequence LIMIT 1)
 ELSE
  (SELECT rowid FROM output_chunk WHERE execution_id=o.id AND sequence>w.sequence ORDER BY sequence LIMIT 1)
 END
 WHERE w.witnesses<?2 AND w.owner<?3 AND w.step<?4
) SELECT owner,size FROM walk WHERE has_chunk ORDER BY step";
    let mut statement = db.prepare(sql).map_err(sql_error)?;
    let mut rows = statement
        .query(rusqlite::params![
            owners,
            (budget + 1) as i64,
            ids.len() as i64,
            (budget + ids.len() + 2) as i64
        ])
        .map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let owner = row.get::<_, i64>(0).map_err(sql_error)? as usize;
        out[owner].push(row.get(1).map_err(sql_error)?);
    }
    Ok(out)
}

fn add_bytes(total: &mut u64, value: u64, overflow: &mut bool) {
    match total.checked_add(value) {
        Some(sum) => *total = sum,
        None => {
            *total = u64::MAX;
            *overflow = true;
        }
    }
}
pub(crate) fn prepare(raw: RawStorageObservation) -> StorageObservation {
    let category = |kind| {
        let mut sample = BytesObservation {
            bytes: 0,
            complete: true,
            examined_rows: 0,
            limited_reason: None,
        };
        let mut overflow = false;
        for prefix in raw.prefixes.iter().filter(|prefix| prefix.category == kind) {
            for value in &prefix.bytes {
                add_bytes(&mut sample.bytes, *value, &mut overflow);
            }
            sample.complete &= prefix.complete;
            sample.examined_rows += prefix.examined;
        }
        if overflow {
            sample.complete = false;
            sample.limited_reason =
                Some("byte total exceeds u64; displayed value is a lower bound");
        } else if !sample.complete {
            sample.limited_reason = Some("finite scalar prefix or missing vector relation");
        }
        sample
    };
    let history = category(Category::History);
    let chunks = category(Category::Raw);
    let index = category(Category::Index);
    let managed = category(Category::Managed);
    let mut logical = BytesObservation {
        bytes: 0,
        complete: true,
        examined_rows: 0,
        limited_reason: None,
    };
    let mut logical_overflow = false;
    for part in [&history, &chunks, &index, &managed] {
        add_bytes(&mut logical.bytes, part.bytes, &mut logical_overflow);
        logical.complete &= part.complete;
        logical.examined_rows += part.examined_rows;
    }
    if logical_overflow {
        logical.complete = false;
        logical.limited_reason =
            Some("logical total exceeds u64; displayed value is a lower bound");
    } else if !logical.complete {
        logical.limited_reason = Some("one or more logical byte categories are incomplete");
    }
    let mut eligible = 0u64;
    let mut expired = 0u64;
    let mut witnessed = 0u64;
    let mut witnessed_rows = 0usize;
    let mut raw_overflow = false;
    let mut known = raw.executions_complete;
    for execution in &raw.executions {
        known &= execution.complete;
        witnessed_rows += execution.bytes.len();
        let mut bytes = 0u64;
        for size in &execution.bytes {
            add_bytes(&mut bytes, *size, &mut raw_overflow);
        }
        add_bytes(&mut witnessed, bytes, &mut raw_overflow);
        if !execution.terminal || execution.pinned {
            continue;
        }
        add_bytes(&mut eligible, bytes, &mut raw_overflow);
        let aged = if let Some(cutoff) = raw.cutoff {
            match execution
                .ended
                .as_ref()
                .or(execution.started.as_ref())
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            {
                Some(at) => at.with_timezone(&Utc) <= cutoff,
                None => {
                    known = false;
                    false
                }
            }
        } else {
            false
        };
        if execution.marked || aged {
            add_bytes(&mut expired, bytes, &mut raw_overflow);
        }
    }
    let covered =
        chunks.complete && witnessed == chunks.bytes && witnessed_rows == chunks.examined_rows;
    let complete = known && covered && !raw_overflow;
    let reclaimable = BytesObservation {
        bytes: if complete {
            expired
                .max(chunks.bytes.saturating_sub(raw.quota))
                .min(eligible)
        } else {
            expired
        },
        complete,
        examined_rows: raw.raw_examined,
        limited_reason: if raw_overflow {
            Some("raw byte evidence exceeds u64; quota excess is unverified")
        } else if !covered {
            Some(
                "raw execution witnesses do not completely cover saved output; quota excess is unverified",
            )
        } else if !known {
            Some("raw eligibility or age evidence is incomplete; quota excess is unverified")
        } else {
            None
        },
    };
    StorageObservation {
        observed_at: raw.at.to_rfc3339(),
        history,
        raw: chunks,
        index,
        managed,
        logical,
        reusable_bytes: raw.reusable,
        reclaimable,
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PhysicalLimits {
    pub entries_per_directory: usize,
    pub total_entries: usize,
}
impl Default for PhysicalLimits {
    fn default() -> Self {
        Self {
            entries_per_directory: 1024,
            total_entries: 3072,
        }
    }
}
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct PhysicalField {
    pub bytes: Option<u64>,
    pub complete: bool,
    pub entries: usize,
    pub started_at: String,
    pub observed_at: String,
    pub error: Option<String>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct PhysicalSample {
    pub started_at: String,
    pub observed_at: String,
    pub database: PhysicalField,
    pub wal: PhysicalField,
    pub managed_clipboard: PhysicalField,
    pub temporary: PhysicalField,
    pub logs: PhysicalField,
    pub filesystem_free: PhysicalField,
}
fn field(
    started_at: String,
    bytes: Option<u64>,
    complete: bool,
    entries: usize,
    error: Option<String>,
) -> PhysicalField {
    PhysicalField {
        bytes,
        complete,
        entries,
        started_at,
        observed_at: Utc::now().to_rfc3339(),
        error,
    }
}
fn stopped(deadline: Instant, cancel: &CancellationToken) -> Option<String> {
    if cancel.is_cancelled() {
        Some("physical sample cancelled".into())
    } else if Instant::now() >= deadline {
        Some("physical sample soft deadline reached; an OS call may outlive it".into())
    } else {
        None
    }
}
fn fixed(path: &Path, deadline: Instant, cancel: &CancellationToken) -> PhysicalField {
    let at = Utc::now().to_rfc3339();
    if let Some(reason) = stopped(deadline, cancel) {
        return field(at, None, false, 0, Some(reason));
    }
    match std::fs::symlink_metadata(path) {
        // Inode size preserves the CLI's symlink-file accounting without following it.
        Ok(metadata) => field(at, Some(metadata.len()), true, 0, None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            field(at, Some(0), true, 0, None)
        }
        Err(error) => field(at, None, false, 0, Some(error.to_string())),
    }
}
fn directory(
    path: &Path,
    limit: usize,
    total: &mut usize,
    deadline: Instant,
    cancel: &CancellationToken,
) -> PhysicalField {
    let at = Utc::now().to_rfc3339();
    if let Some(reason) = stopped(deadline, cancel) {
        return field(at, None, false, 0, Some(reason));
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return field(
                at,
                None,
                false,
                0,
                Some("symlink directory is not traversed".into()),
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return field(at, Some(0), true, 0, None);
        }
        Err(error) => return field(at, None, false, 0, Some(error.to_string())),
    }
    if let Some(reason) = stopped(deadline, cancel) {
        return field(at, None, false, 0, Some(reason));
    }
    let mut entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => return field(at, None, false, 0, Some(error.to_string())),
    };
    let mut bytes = 0u64;
    let mut examined = 0;
    loop {
        if let Some(reason) = stopped(deadline, cancel) {
            return field(at, Some(bytes), false, examined, Some(reason));
        }
        // At the exact cap, keep incompleteness explicit rather than doing one
        // more filesystem operation to prove an absent sentinel.
        if examined == limit || *total == 0 {
            return field(
                at,
                Some(bytes),
                false,
                examined,
                Some("directory entry limit reached".into()),
            );
        }
        let entry = match entries.next() {
            None => return field(at, Some(bytes), true, examined, None),
            Some(Ok(entry)) => entry,
            Some(Err(error)) => {
                return field(at, Some(bytes), false, examined, Some(error.to_string()));
            }
        };
        examined += 1;
        *total -= 1;
        if let Some(reason) = stopped(deadline, cancel) {
            return field(at, Some(bytes), false, examined, Some(reason));
        }
        match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    bytes = bytes.saturating_add(metadata.len());
                }
            }
            Err(error) => return field(at, Some(bytes), false, examined, Some(error.to_string())),
        }
    }
}
/// Synchronous finite closure; use `sample_physical`/the existing BlockingFiles pool.
pub(crate) fn physical_sample(
    directory_path: &Path,
    limits: PhysicalLimits,
    deadline: Instant,
    cancel: &CancellationToken,
) -> PhysicalSample {
    let at = Utc::now().to_rfc3339();
    let per = limits.entries_per_directory.min(1024);
    let mut total = limits.total_entries.min(3072);
    let database = fixed(&directory_path.join("shellboy.db"), deadline, cancel);
    let wal = fixed(&directory_path.join("shellboy.db-wal"), deadline, cancel);
    let managed_at = Utc::now().to_rfc3339();
    let managed_clipboard = if let Some(reason) = stopped(deadline, cancel) {
        field(managed_at, None, false, 0, Some(reason))
    } else {
        match std::fs::symlink_metadata(directory_path.join("managed")) {
            Ok(meta) if meta.file_type().is_symlink() => field(
                managed_at,
                None,
                false,
                0,
                Some("symlink parent is not traversed".into()),
            ),
            Ok(_) => directory(
                &directory_path.join("managed/clipboard"),
                per,
                &mut total,
                deadline,
                cancel,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                field(managed_at, Some(0), true, 0, None)
            }
            Err(error) => field(managed_at, None, false, 0, Some(error.to_string())),
        }
    };
    let temporary = directory(
        &directory_path.join("tmp"),
        per,
        &mut total,
        deadline,
        cancel,
    );
    let logs = directory(
        &directory_path.join("logs"),
        per,
        &mut total,
        deadline,
        cancel,
    );
    let start = Utc::now().to_rfc3339();
    let filesystem_free = if let Some(reason) = stopped(deadline, cancel) {
        field(start, None, false, 0, Some(reason))
    } else {
        match fs2::available_space(directory_path) {
            Ok(bytes) => field(start, Some(bytes), true, 0, None),
            Err(error) => field(start, None, false, 0, Some(error.to_string())),
        }
    };
    PhysicalSample {
        started_at: at,
        observed_at: Utc::now().to_rfc3339(),
        database,
        wal,
        managed_clipboard,
        temporary,
        logs,
        filesystem_free,
    }
}
pub(crate) async fn sample_physical(
    files: &BlockingFiles,
    path: PathBuf,
    limits: PhysicalLimits,
    deadline: tokio::time::Instant,
    cancel: CancellationToken,
) -> Result<PhysicalSample, ApiError> {
    let closure_cancel = cancel.clone();
    files
        .execute(deadline, cancel, move || {
            Ok(physical_sample(
                &path,
                limits,
                deadline.into_std(),
                &closure_cancel,
            ))
        })
        .await
}

impl super::GcService {
    /// UI observation only; the existing exact CLI details path remains unchanged.
    pub(crate) async fn sample_observation_physical(
        &self,
        deadline: tokio::time::Instant,
        cancel: CancellationToken,
    ) -> Result<PhysicalSample, ApiError> {
        sample_physical(
            &self.files,
            self.data_dir.clone(),
            PhysicalLimits::default(),
            deadline,
            cancel,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::time::Duration;
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db
    }
    fn execution(db: &Connection, id: &str, state: &str) {
        db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at)VALUES(?1,'r','u',?1,?2,'/','{}','UTF-8','UTF-8','closed','2020-01-01T00:00:00Z','2020-01-01T00:00:01Z')",params![id,state]).unwrap();
    }
    fn chunk(db: &Connection, id: &str, sequence: i64, bytes: &[u8]) {
        db.execute(
            "INSERT INTO output_chunk VALUES(?1,?2,'stdout',?2,?3,'2020-01-01T00:00:00Z')",
            params![id, sequence, bytes],
        )
        .unwrap();
    }
    #[test]
    fn batch_keeps_empty_and_unconfirmed_owners_and_one_shared_boundary_witness() {
        let db = db();
        execution(&db, "empty", "exited");
        execution(&db, "unconfirmed", "running");
        execution(&db, "tail", "exited");
        chunk(&db, "unconfirmed", 9, b"x");
        chunk(&db, "unconfirmed", 2, b"xx");
        chunk(&db, "tail", 0, b"xxx");
        let ids = [Some("empty"), Some("unconfirmed"), Some("tail")];
        let trace = crate::frontend::sql_trace::Statements::new(&db);
        let sampled = output_samples(&db, &ids, 2).unwrap();
        assert_eq!(trace.count(), 1);
        assert_eq!(sampled, vec![vec![], vec![Some(2), Some(1)], vec![Some(3)]]);
        assert_eq!(sampled.iter().map(Vec::len).sum::<usize>(), 3);
        drop(trace);
        let sampled = output_samples(&db, &ids, 1).unwrap();
        assert_eq!(sampled, vec![vec![], vec![Some(2), Some(1)], vec![]]);
        assert_eq!(
            output_samples(&db, &ids, 0).unwrap(),
            vec![vec![], vec![], vec![]]
        );
    }
    #[test]
    fn small_complete_prefixes_match_full_cli_totals_and_reclaimable_eligibility() {
        let db = db();
        db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('n','message','now','{}');INSERT INTO app_meta(key,value)VALUES('revisit.t','saved revisit'),('index.fixture','saved index');INSERT INTO managed_file(id,absolute_path,state,raw_size,created_at)VALUES('f','/f','ready',17,'now');INSERT INTO index_space VALUES('s','p','m',3,'v','active','v');INSERT INTO embedding_chunk VALUES('c','j','s','n',0,0,1,'hash',1);").unwrap();
        execution(&db, "expired", "exited");
        execution(&db, "pinned", "exited");
        execution(&db, "live", "running");
        chunk(&db, "expired", 0, b"12345");
        chunk(&db, "pinned", 0, b"1234567");
        chunk(&db, "live", 0, b"123");
        db.execute("INSERT INTO runtime_pin(id,instance_id,resource_kind,resource_id,generation,created_at)VALUES('pin','fixture','shell_output','pinned',0,'2020-01-01T00:00:00Z')",[]).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        let raw = read_tx(&tx, Limits::default(), &Retention::default(), Utc::now()).unwrap();
        let result = prepare(raw);
        let exact = super::super::usage::logical_usage(&tx).unwrap();
        assert!(result.logical.complete);
        assert_eq!(result.history.bytes, exact.history);
        assert_eq!(result.raw.bytes, exact.raw);
        assert_eq!(result.index.bytes, exact.index);
        assert_eq!(result.managed.bytes, exact.managed);
        assert_eq!(result.logical.bytes, exact.total());
        assert_eq!(result.reusable_bytes, exact.reusable);
        assert!(result.reclaimable.complete);
        assert_eq!(result.reclaimable.bytes, 5);
        assert!(!result.observed_at.is_empty());
    }
    #[test]
    fn incomplete_prefixes_keep_lower_bounds_without_inventing_zero_or_global_quota_excess() {
        let db = db();
        execution(&db, "x", "exited");
        chunk(&db, "x", 0, b"12345");
        chunk(&db, "x", 1, b"1234567");
        let retention = Retention {
            tool_raw_bytes: 1,
            ..Default::default()
        };
        let result = prepare(
            read_tx(
                &db,
                Limits {
                    rows_per_index: 1,
                    executions: 1,
                    output_witnesses: 1,
                },
                &retention,
                Utc::now(),
            )
            .unwrap(),
        );
        assert!(!result.raw.complete);
        assert_eq!(result.raw.bytes, 5);
        assert!(!result.logical.complete);
        assert!(!result.reclaimable.complete);
        assert_eq!(
            result.reclaimable.bytes, 5,
            "positively expired sampled bytes are safe lower-bound evidence"
        );
        let unknown = prepare(
            read_tx(
                &db,
                Limits {
                    rows_per_index: 0,
                    executions: 0,
                    output_witnesses: 0,
                },
                &retention,
                Utc::now(),
            )
            .unwrap(),
        );
        assert_eq!(unknown.reclaimable.bytes, 0);
        assert!(
            !unknown.reclaimable.complete,
            "zero observed evidence is unknown, not exact zero reclaimable"
        );
    }
    fn measured(db: &Connection) -> (StorageObservation, usize) {
        extern "C" fn progress(context: *mut std::ffi::c_void) -> i32 {
            unsafe {
                *context.cast::<usize>() += 1;
            }
            0
        }
        let mut count = Box::new(0usize);
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                db.handle(),
                100,
                Some(progress),
                std::ptr::from_mut(count.as_mut()).cast(),
            );
        }
        let result = prepare(
            read_tx(
                db,
                Limits {
                    rows_per_index: 16,
                    executions: 16,
                    output_witnesses: 16,
                },
                &Retention::default(),
                Utc::now(),
            )
            .unwrap(),
        );
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(db.handle(), 0, None, std::ptr::null_mut());
        }
        (result, *count * 100)
    }
    #[test]
    fn actual_sql_work_stays_bounded_with_one_hundred_thousand_scalar_rows() {
        let db = db();
        let (_, before) = measured(&db);
        db.execute_batch("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) INSERT INTO node(id,kind,created_at,payload_json) SELECT 'n'||x,'message','now','{}' FROM n;").unwrap();
        let (result, steps) = measured(&db);
        assert!(!result.history.complete);
        assert!(result.history.examined_rows <= 10 * 17);
        assert!(
            steps < before + 20000,
            "VM growth exceeded a finite prefix: before {before}, after {steps}"
        );
        for (table, index, expression, predicate, _) in SCALARS {
            let query = scalar_sql(table, index, expression, predicate);
            let mut plan = db.prepare(&format!("EXPLAIN QUERY PLAN {query}")).unwrap();
            let details = plan
                .query_map([17], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(
                !details.iter().any(|detail| detail.contains("TEMP B-TREE")),
                "unbounded prefix sort: {query}: {details:?}"
            );
        }
        execution(&db, "raw-head", "running");
        db.execute_batch("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) SELECT 'e'||x,'r','u','o'||x,'running','/','{}','UTF-8','UTF-8','closed','2020-01-01T00:00:00Z' FROM n;").unwrap();
        let trace = crate::frontend::sql_trace::Statements::new(&db);
        let empty_raw = read_tx(
            &db,
            Limits {
                rows_per_index: 16,
                executions: 16,
                output_witnesses: 16,
            },
            &Retention::default(),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(
            trace.count(),
            18,
            "execution prefixes stop before eligibility filtering; one streaming batch"
        );
        drop(trace);
        assert_eq!(empty_raw.executions.len(), 16);
        assert!(!empty_raw.executions_complete);
        let (_, execution_steps) = measured(&db);
        assert!(
            execution_steps < steps + 20000,
            "100k unrelated executions increased VM work: {steps}->{execution_steps}"
        );
        db.execute_batch("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at) SELECT 'raw-head',x,'stdout',x,x'78','2020-01-01T00:00:00Z' FROM n;").unwrap();
        let trace = crate::frontend::sql_trace::Statements::new(&db);
        let sampled = read_tx(
            &db,
            Limits {
                rows_per_index: 16,
                executions: 16,
                output_witnesses: 16,
            },
            &Retention::default(),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(
            trace.count(),
            18,
            "one PK followup consumes the combined witness budget including one sentinel"
        );
        drop(trace);
        assert_eq!(sampled.raw_examined, 17);
        assert_eq!(sampled.executions[0].bytes.len(), 16);
        assert!(!sampled.executions[0].complete);
        let (raw_result, raw_steps) = measured(&db);
        assert!(
            raw_steps < execution_steps + 20000,
            "100k output chunks increased VM work: {execution_steps}->{raw_steps}"
        );
        assert!(!raw_result.raw.complete);
        assert!(!raw_result.reclaimable.complete);
        assert_eq!(
            raw_result.reclaimable.bytes, 0,
            "sampled running output cannot become reclaimable"
        );
        assert!(
            read_tx(
                &db,
                Limits {
                    rows_per_index: 1025,
                    ..Limits::default()
                },
                &Retention::default(),
                Utc::now()
            )
            .is_err()
        );
    }
    #[test]
    fn physical_sampling_has_separate_timestamps_finite_entries_and_unknown_cancellation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("shellboy.db"), b"database").unwrap();
        std::fs::create_dir(root.path().join("logs")).unwrap();
        for n in 0..20 {
            std::fs::write(root.path().join("logs").join(format!("{n}")), b"1234").unwrap();
        }
        let sample = physical_sample(
            root.path(),
            PhysicalLimits {
                entries_per_directory: 3,
                total_entries: 3,
            },
            crate::test_watchdog::Deadline::new("bounded storage file observation").at(),
            &CancellationToken::new(),
        );
        assert_eq!(sample.database.bytes, Some(8));
        assert!(sample.database.complete);
        assert_eq!(sample.wal.bytes, Some(0));
        assert!(sample.wal.complete);
        assert_eq!(sample.logs.entries, 3);
        assert_eq!(sample.logs.bytes, Some(12));
        assert!(!sample.logs.complete);
        assert!(sample.logs.error.is_some());
        assert!(!sample.started_at.is_empty());
        assert!(!sample.logs.observed_at.is_empty());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let unknown = physical_sample(
            root.path(),
            PhysicalLimits::default(),
            crate::test_watchdog::Deadline::new("bounded storage file observation").at(),
            &cancel,
        );
        assert_eq!(unknown.database.bytes, None);
        assert!(!unknown.database.complete);
        assert_eq!(unknown.filesystem_free.bytes, None);
        let expired = physical_sample(
            root.path(),
            PhysicalLimits::default(),
            Instant::now() - Duration::from_millis(1),
            &CancellationToken::new(),
        );
        assert_eq!(expired.database.bytes, None);
        // A regular file where a directory is expected is an error, not zero.
        std::fs::write(root.path().join("tmp"), b"not a directory").unwrap();
        let error = physical_sample(
            root.path(),
            PhysicalLimits::default(),
            crate::test_watchdog::Deadline::new("bounded storage file observation").at(),
            &CancellationToken::new(),
        );
        assert_eq!(error.temporary.bytes, None);
        assert!(!error.temporary.complete);
    }
    #[cfg(unix)]
    #[test]
    fn physical_sampling_never_traverses_symlink_roots_or_nested_directories() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("clipboard")).unwrap();
        std::fs::write(outside.path().join("clipboard/big"), vec![0u8; 10000]).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("managed")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("shellboy.db")).unwrap();
        std::fs::create_dir_all(root.path().join("logs/nested")).unwrap();
        std::fs::write(root.path().join("logs/nested/big"), vec![0u8; 10000]).unwrap();
        let sample = physical_sample(
            root.path(),
            PhysicalLimits::default(),
            crate::test_watchdog::Deadline::new("bounded storage file observation").at(),
            &CancellationToken::new(),
        );
        assert_eq!(
            sample.database.bytes,
            Some(
                std::fs::symlink_metadata(root.path().join("shellboy.db"))
                    .unwrap()
                    .len()
            )
        );
        assert!(sample.database.complete);
        assert_eq!(sample.managed_clipboard.bytes, None);
        assert_eq!(sample.logs.bytes, Some(0));
        assert!(sample.logs.complete);
    }
    #[tokio::test]
    async fn physical_wrapper_uses_existing_finite_file_pool() {
        let root = tempfile::tempdir().unwrap();
        let files = BlockingFiles::new(1);
        let sample = sample_physical(
            &files,
            root.path().to_path_buf(),
            PhysicalLimits::default(),
            tokio::time::Instant::from_std(
                crate::test_watchdog::Deadline::new("bounded storage file observation").at(),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(sample.database.bytes, Some(0));
        assert!(sample.database.complete);
        files.wait_idle().await.unwrap();
    }
    #[test]
    fn saved_vector_sum_overflow_is_never_an_exact_logical_total() {
        let db = db();
        db.execute_batch("INSERT INTO index_space VALUES('huge','p','m',4000000000000000000,'v','active','v');INSERT INTO embedding_chunk VALUES('a','j','huge','n',0,0,1,'h',1),('b','j','huge','n',0,0,1,'h',2);").unwrap();
        let result =
            prepare(read_tx(&db, Limits::default(), &Retention::default(), Utc::now()).unwrap());
        assert_eq!(result.index.bytes, u64::MAX);
        assert!(!result.index.complete);
        assert!(result.index.limited_reason.unwrap().contains("u64"));
        assert!(!result.logical.complete);
    }
    #[test]
    fn orphan_output_cannot_inflate_quota_excess_or_hide_positive_expired_evidence() {
        let db = db();
        execution(&db, "young", "exited");
        let now = Utc::now().to_rfc3339();
        db.execute(
            "UPDATE shell_execution SET started_at=?1,ended_at=?1 WHERE id='young'",
            [now],
        )
        .unwrap();
        chunk(&db, "young", 0, b"12345");
        chunk(&db, "orphan", 0, &[0u8; 100]);
        let retention = Retention {
            tool_raw_bytes: 10,
            ..Default::default()
        };
        let result = prepare(read_tx(&db, Limits::default(), &retention, Utc::now()).unwrap());
        assert!(result.raw.complete);
        assert_eq!(result.raw.bytes, 105);
        assert!(!result.reclaimable.complete);
        assert_eq!(result.reclaimable.bytes, 0);
        assert!(result.reclaimable.limited_reason.unwrap().contains("cover"));
        execution(&db, "expired", "exited");
        chunk(&db, "expired", 0, b"seven77");
        let result = prepare(read_tx(&db, Limits::default(), &retention, Utc::now()).unwrap());
        assert!(!result.reclaimable.complete);
        assert_eq!(
            result.reclaimable.bytes, 7,
            "only positively expired unpinned saved evidence is a safe lower bound"
        );
    }
}
