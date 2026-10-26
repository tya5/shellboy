//! SQLite metadata, space/witness validation, and transactional vector storage.
use super::*;

#[derive(Clone, Debug)]
pub(super) struct Space {
    pub(super) id: String,
    pub(super) profile: String,
    pub(super) model: String,
    pub(super) dimension: usize,
    pub(super) splitter: String,
    pub(super) state: String,
    pub(super) table: String,
}
pub(super) use crate::history::RawWitness;
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(super) struct Witness {
    pub(super) node_id: String,
    pub(super) revision: i64,
    pub(super) body_hash: Option<String>,
    pub(super) raw: Option<RawWitness>,
}
impl Witness {
    pub(super) fn from_watermark(body: &str) -> Option<Self> {
        serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|value| value.get("witness").cloned())
            .and_then(|value| serde_json::from_value(value).ok())
    }

    pub(super) fn matches_body(&self, other: &Self, kind: &str) -> bool {
        self.node_id == other.node_id
            && self.revision == other.revision
            && self.body_hash == other.body_hash
            && (kind != "tool_raw" || self.raw.is_some() && self.raw == other.raw)
    }
    pub(super) fn canonical(&self) -> Self {
        Self {
            raw: None,
            ..self.clone()
        }
    }
}
pub(super) fn meta(db: &Connection, key: &str) -> Result<Option<String>, StorageError> {
    db.query_row("SELECT value FROM app_meta WHERE key=?1", [key], |row| {
        row.get(0)
    })
    .optional()
    .map_err(StorageError::from)
}
pub(super) fn set_meta(tx: &Transaction<'_>, key: &str, value: &str) -> Result<(), StorageError> {
    let app_meta_sql = "INSERT INTO app_meta(key,value)VALUES(?1,?2)ON CONFLICT(key)DO UPDATE SET value=excluded.value";
    tx.execute(app_meta_sql, params![key, value])?;
    Ok(())
}
/// Keep strict malformed-record classification local; callers that deliberately
/// tolerate legacy/corrupt cache entries explicitly choose `.ok()`.
pub(super) fn decode_json<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, StorageError> {
    serde_json::from_str(body).map_err(|_| serde_error())
}
pub(super) fn encode_json<T: Serialize + ?Sized>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|_| serde_error())
}
pub(super) fn json_meta<T: serde::de::DeserializeOwned>(
    db: &Connection,
    key: &str,
) -> Result<Option<T>, StorageError> {
    meta(db, key)?.as_deref().map(decode_json).transpose()
}
pub(super) fn set_json_meta<T: Serialize>(
    tx: &Transaction<'_>,
    key: &str,
    value: &T,
) -> Result<(), StorageError> {
    set_meta(tx, key, &encode_json(value)?)
}
pub(super) fn update_json_meta<T: Serialize>(
    db: &Connection,
    key: &str,
    value: &T,
) -> Result<usize, StorageError> {
    db.execute(
        "UPDATE app_meta SET value=?2 WHERE key=?1",
        params![key, encode_json(value)?],
    )
    .map_err(StorageError::from)
}
pub(super) fn valid_table(table: &str) -> Result<(), StorageError> {
    let Some(suffix) = table.strip_prefix("shellboy_vec_") else {
        return Err(StorageError::Schema(
            "Invalid generated vector table identifier".into(),
        ));
    };
    if suffix.len() != 32 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(StorageError::Schema(
            "Invalid generated vector table identifier".into(),
        ));
    }
    Ok(())
}
pub(super) fn load_space(db: &Connection, id: &str) -> Result<Option<Space>, StorageError> {
    let index_space_sql = "SELECT id,profile_id,model_id,dimension,splitter_version,state,vec_table_name FROM index_space WHERE id=?1";
    db.query_row(index_space_sql, [id], |row| {
        Ok(Space {
            id: row.get(0)?,
            profile: row.get(1)?,
            model: row.get(2)?,
            dimension: row.get::<_, i64>(3)? as usize,
            splitter: row.get(4)?,
            state: row.get(5)?,
            table: row.get(6)?,
        })
    })
    .optional()
    .map_err(StorageError::from)
}
pub(super) fn group_current(
    db: &Connection,
    keys: IndexKeys<'_>,
    group: &str,
) -> Result<bool, StorageError> {
    if group == "active" {
        Ok(true)
    } else {
        Ok(meta(db, &keys.rebuild())?.as_deref() == Some(group))
    }
}
pub(super) fn space_for_group(
    db: &Connection,
    keys: IndexKeys<'_>,
    group: &str,
) -> Result<Option<Space>, StorageError> {
    let pointer = if group == "active" {
        IndexKeys::ACTIVE_SPACE.to_owned()
    } else {
        keys.group(group)
    };
    let primary = meta(db, &pointer)?
        .map(|id| load_space(db, &id))
        .transpose()?
        .flatten();
    let compatible = |s: &Space| {
        s.splitter == keys.splitter()
            && matches!(
                s.state.as_str(),
                "active" | "paused" | "candidate" | "candidate_paused"
            )
    };
    if let Some(space) = primary.filter(compatible) {
        return Ok(Some(space));
    }
    if group == "active" {
        return Ok(meta(db, &keys.group("active"))?
            .map(|id| load_space(db, &id))
            .transpose()?
            .flatten()
            .filter(compatible));
    }
    Ok(None)
}

pub(super) fn ensure_space(
    tx: &Transaction<'_>,
    embedding: &identity::EmbeddingIdentity,
    group: &str,
    dimension: usize,
    activate_active: bool,
) -> Result<Space, StorageError> {
    let index_space_sql = "INSERT INTO index_space(id,profile_id,model_id,dimension,splitter_version,state,vec_table_name)VALUES(?1,?2,?3,?4,?5,?6,?7)";
    let keys = embedding.keys();
    let (profile, model) = (&embedding.profile, &embedding.model);
    let splitter = keys.splitter();
    if let Some(space) = space_for_group(tx, keys, group)? {
        if space.dimension != dimension {
            return Err(StorageError::from(err(
                ErrorCode::InvalidState,
                "Embedding dimension changed; explicitly rebuild into a new space",
            )));
        }
        return Ok(space);
    }
    if dimension == 0 || dimension > crate::storage::MAX_VECTOR_DIMENSIONS {
        return Err(StorageError::from(err(
            ErrorCode::IoFailed,
            "sqlite-vec0 v0.1.9 supports dimensions1..=8192",
        )));
    }
    let space = id();
    let table = format!("shellboy_vec_{}", uuid::Uuid::new_v4().simple());
    valid_table(&table)?;
    tx.execute_batch(&format!(
        "CREATE VIRTUAL TABLE {table} USING vec0(embedding float[{dimension}]);"
    ))?;
    let state = if group == "active" && activate_active {
        "active"
    } else {
        "candidate"
    };
    if group == "active" && activate_active {
        tx.execute(
            "UPDATE index_space SET state='inactive' WHERE state IN ('active','paused')",
            [],
        )?;
        set_meta(tx, IndexKeys::ACTIVE_SPACE, &space)?;
    } else {
        set_meta(tx, &keys.group(group), &space)?;
    }
    tx.execute(
        index_space_sql,
        params![
            space,
            profile,
            model,
            dimension as i64,
            splitter,
            state,
            table
        ],
    )?;
    Ok(Space {
        id: space,
        profile: profile.into(),
        model: model.into(),
        dimension,
        splitter,
        state: state.into(),
        table,
    })
}
pub(super) fn witness_valid(
    db: &Connection,
    witness: &Witness,
    kind: &str,
) -> Result<bool, StorageError> {
    let node_sql = "SELECT payload_revision,body_hash,payload_json IS NOT NULL,deleted_reason FROM node WHERE id=?1";
    let current: Option<(i64, Option<String>, bool, Option<String>)> = db
        .query_row(node_sql, [&witness.node_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?;
    let Some((revision, body_hash, true, None)) = current else {
        return Ok(false);
    };
    if revision != witness.revision || body_hash != witness.body_hash {
        return Ok(false);
    }
    if kind == "tool_raw" {
        let Some(raw) = &witness.raw else {
            return Ok(false);
        };
        let valid = raw.is_current(db)?;
        if !valid {
            return Ok(false);
        }
    }
    Ok(true)
}
pub(super) fn adopt_vector_tx(
    tx: &Transaction<'_>,
    space: &Space,
    job: &str,
    work: &Work,
    vector: &[f32],
) -> Result<(), StorageError> {
    let embedding_chunk_sql = "INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id)VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)";
    let embedding_job_sql =
        "UPDATE embedding_job SET state='done',error_json=NULL,next_attempt_at=NULL WHERE id=?1";
    if !witness_valid(tx, &work.witness, &work.body_kind)? {
        return Err(StorageError::from(err(
            ErrorCode::InvalidState,
            "Index source changed before vector adoption",
        )));
    }
    valid_table(&space.table)?;
    if vector.len() != space.dimension || vector.iter().any(|v| !v.is_finite()) {
        return Err(StorageError::from(err(
            ErrorCode::IoFailed,
            "Invalid embedding dimension or non-finite vector",
        )));
    }
    let row: i64 = tx.query_row(
        "SELECT COALESCE(MAX(vec_row_id),0)+1 FROM embedding_chunk WHERE space_id=?1",
        [&space.id],
        |row| row.get(0),
    )?;
    let encoded = serde_json::to_string(vector).map_err(|_| serde_error())?;
    tx.execute(
        &format!("INSERT INTO {}(rowid,embedding)VALUES(?1,?2)", space.table),
        params![row, encoded],
    )?;
    let chunk = id();
    tx.execute(
        embedding_chunk_sql,
        params![
            chunk,
            job,
            space.id,
            work.witness.node_id,
            work.witness.revision,
            work.segment.start as i64,
            work.segment.end as i64,
            hash(work.segment.text.as_bytes()),
            row
        ],
    )?;
    set_meta(
        tx,
        &IndexKeys::preview(&chunk),
        &work.segment.text.chars().take(512).collect::<String>(),
    )?;
    tx.execute(embedding_job_sql, [job])?;
    tx.execute("DELETE FROM app_meta WHERE key=?1", [IndexKeys::input(job)])?;
    Ok(())
}
pub(super) fn maybe_activate_candidate(
    tx: &Transaction<'_>,
    keys: IndexKeys<'_>,
    group: &str,
) -> Result<(), StorageError> {
    finish_candidate(tx, keys, group, true)
}
pub(super) fn finish_candidate(
    tx: &Transaction<'_>,
    keys: IndexKeys<'_>,
    group: &str,
    activate: bool,
) -> Result<(), StorageError> {
    let embedding_job_sql = "SELECT EXISTS(SELECT 1 FROM embedding_job WHERE space_id=?1 AND state NOT IN ('done','split'))";
    if (group == "active" && activate)
        || !group_current(tx, keys, group)?
        || meta(tx, &keys.enqueueing(group))?.is_some()
    {
        return Ok(());
    }
    let auto_pending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key LIKE ?1)",
        [keys.auto_pattern()],
        |row| row.get(0),
    )?;
    if auto_pending && group != "active" {
        return Ok(());
    }
    let Some(space) = space_for_group(tx, keys, group)? else {
        return Ok(());
    };
    let unfinished: bool = tx.query_row(embedding_job_sql, [&space.id], |r| r.get(0))?;
    if unfinished {
        return Ok(());
    }
    let mut statement = tx.prepare("SELECT value FROM app_meta WHERE key LIKE ?1")?;
    for row in statement.query_map([keys.bootstrap_pattern()], |r| r.get::<_, String>(0))? {
        let work: Work = decode_json(&row?)?;
        if work.group == group {
            return Ok(());
        }
    }
    drop(statement);
    if activate {
        tx.execute(
            "UPDATE index_space SET state='inactive' WHERE state IN ('active','paused')",
            [],
        )?;
        tx.execute(
            "UPDATE index_space SET state='active' WHERE id=?1",
            [&space.id],
        )?;
        set_meta(tx, IndexKeys::ACTIVE_SPACE, &space.id)?;
    } else {
        tx.execute(
            "UPDATE index_space SET state='inactive' WHERE id=?1",
            [&space.id],
        )?;
        if group == "active" {
            tx.execute("DELETE FROM app_meta WHERE key=?1", [keys.group("active")])?;
        }
    }
    if group != "active" {
        tx.execute("DELETE FROM app_meta WHERE key=?1", [keys.rebuild()])?;
    }
    Ok(())
}
/// Delete every derived body in the transaction that tombstones source text.
pub fn delete_source_vectors_tx(tx: &Transaction<'_>, node_id: &str) -> Result<(), ApiError> {
    delete_vectors_tx(tx, node_id, false)
}
/// Raw retention/decoding never invalidates immutable canonical quotes or paths.
pub(crate) fn delete_raw_vectors_tx(tx: &Transaction<'_>, node_id: &str) -> Result<(), ApiError> {
    delete_vectors_tx(tx, node_id, true)
}
fn delete_vectors_tx(tx: &Transaction<'_>, node_id: &str, raw_only: bool) -> Result<(), ApiError> {
    let embedding_chunk_sql = "SELECT c.id,c.vec_row_id,s.vec_table_name FROM embedding_chunk c JOIN index_space s ON s.id=c.space_id WHERE c.source_node_id=?1 AND (NOT ?2 OR EXISTS(SELECT 1 FROM embedding_job j WHERE j.id=c.job_id AND j.body_kind='tool_raw'))";
    let embedding_job_sql =
        "SELECT id FROM embedding_job WHERE source_node_id=?1 AND (NOT ?2 OR body_kind='tool_raw')";
    let embedding_chunk_sql_2 = "DELETE FROM embedding_chunk WHERE source_node_id=?1 AND (NOT ?2 OR job_id IN (SELECT id FROM embedding_job WHERE source_node_id=?1 AND body_kind='tool_raw'))";
    let embedding_job_sql_2 =
        "DELETE FROM embedding_job WHERE source_node_id=?1 AND (NOT ?2 OR body_kind='tool_raw')";
    let mut statement = tx.prepare(embedding_chunk_sql).map_err(sql_error)?;
    let rows = statement
        .query_map(params![node_id, raw_only], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    drop(statement);
    for (chunk, row, table) in rows {
        valid_table(&table).map_err(db_error)?;
        tx.execute(&format!("DELETE FROM {table} WHERE rowid=?1"), [row])
            .map_err(sql_error)?;
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::preview(&chunk)],
        )
        .map_err(sql_error)?;
    }
    let mut statement = tx.prepare(embedding_job_sql).map_err(sql_error)?;
    let jobs = statement
        .query_map(params![node_id, raw_only], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    drop(statement);
    for job in jobs {
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::input(&job)],
        )
        .map_err(sql_error)?;
    }
    tx.execute(embedding_chunk_sql_2, params![node_id, raw_only])
        .map_err(sql_error)?;
    tx.execute(embedding_job_sql_2, params![node_id, raw_only])
        .map_err(sql_error)?;
    if raw_only {
        // Keep the indexed node lookup used by full source deletion (G11).
        tx.execute(
            &format!("{DELETE_BOOTSTRAP_SOURCE} AND json_extract(value,'$.body_kind')='tool_raw'"),
            [node_id],
        )
        .map_err(sql_error)?;
    } else {
        tx.execute(DELETE_BOOTSTRAP_SOURCE, [node_id])
            .map_err(sql_error)?;
        tx.execute(DELETE_AUTO_SOURCE, [node_id])
            .map_err(sql_error)?;
    }
    Ok(())
}

/// One successful API result and its fixed authorization, owned by
/// the existing DB job. No new durable state or provider retry is introduced.
pub(super) struct EmbeddingAdoption {
    pub(super) failure_key: String,
    pub(super) claimed: Claimed,
    pub(super) embedding: identity::EmbeddingIdentity,
    pub(super) dimension: usize,
    pub(super) vector: Vec<f32>,
    pub(super) legacy: bool,
}
impl EmbeddingAdoption {
    pub(super) fn adopt_embedding_tx(&self, tx: &Transaction<'_>) -> Result<(), StorageError> {
        let claimed = &self.claimed;
        let key_copy = self.embedding.key.as_str();
        let failure_key = self.failure_key.as_str();
        let dimension = self.dimension;
        let vector = self.vector.as_slice();
        let legacy = self.legacy;
        if !claim_valid(tx, claimed)?
            || !witness_valid(tx, &claimed.work.witness, &claimed.work.body_kind)?
            || !group_current(tx, IndexKeys::new(key_copy), &claimed.work.group)?
        {
            discard_claim(tx, claimed, "Source or index group changed before adoption")?;
            return Ok(());
        }
        let preserve_selection = !legacy
            && claimed.work.group == "active"
            && identity::preserve_legacy_selection(tx, failure_key)?;
        let activate = !preserve_selection
            && (!legacy || identity::legacy_may_activate(tx, IndexKeys::new(key_copy))?);
        let space = match &claimed.space {
            Some(space) => load_space(tx, &space.id)?.ok_or_else(serde_error)?,
            None => ensure_space(
                tx,
                &self.embedding,
                &claimed.work.group,
                dimension,
                activate,
            )?,
        };
        if !matches!(space.state.as_str(), "active" | "candidate") {
            claim::defer_claim(tx, claimed, &space.state)?;
            return Ok(());
        }
        if vector.len() != space.dimension || vector.iter().any(|v| !v.is_finite()) {
            return Err(StorageError::from(err(
                ErrorCode::IoFailed,
                "Embedding vector dimension/finite validation failed",
            )));
        }
        let job = if let Some(job) = &claimed.job_id {
            job.clone()
        } else {
            let job = queue_job(tx, &space, &claimed.work)?;
            let job_id = job_id_for(tx, &space, &claimed.work)?.ok_or_else(serde_error)?;
            if !job {
                let state: String = tx.query_row(
                    "SELECT state FROM embedding_job WHERE id=?1",
                    [&job_id],
                    |r| r.get(0),
                )?;
                if state == "done" {
                    if let Some(bootstrap) = &claimed.bootstrap_key {
                        tx.execute("DELETE FROM app_meta WHERE key=?1", [bootstrap])?;
                    }

                    return Ok(());
                }
            }
            job_id
        };
        if claimed.job_id.is_none() {
            tx.execute(
                "UPDATE embedding_job SET attempts=?2 WHERE id=?1",
                params![job, claimed.work.attempts as i64],
            )?;
        }
        adopt_vector_tx(tx, &space, &job, &claimed.work, vector)?;
        if let Some(bootstrap) = &claimed.bootstrap_key {
            tx.execute("DELETE FROM app_meta WHERE key=?1", [bootstrap])?;
        }
        set_meta(tx, &IndexKeys::new(failure_key).common_failures(), "0")?;
        if !preserve_selection {
            finish_candidate(tx, IndexKeys::new(key_copy), &claimed.work.group, activate)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    fn work() -> Work {
        Work::pending(
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
                text: "界".into(),
            },
            "active",
            "explicit",
        )
    }
    #[test]
    fn strict_record_read_and_tolerant_claim_check_keep_distinct_corrupt_policies() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        let missing: Option<Work> = json_meta(&db, "missing").unwrap();
        assert!(missing.is_none());
        db.execute("INSERT INTO app_meta VALUES('input','not JSON')", [])
            .unwrap();
        let error = json_meta::<Work>(&db, "input").unwrap_err();
        assert!(
            matches!(error,StorageError::Worker(ref text) if text=="Stored index job is malformed")
        );
        let claimed = Claimed {
            job_id: None,
            bootstrap_key: Some("input".into()),
            space: None,
            work: work(),
        };
        assert!(
            !claim_valid(&db, &claimed).unwrap(),
            "a corrupt cancelled/stale claim is not a strict record read"
        );
        let legacy = r#"{"witness":{"node_id":"node","revision":1,"body_hash":null,"raw":null},"body_kind":"user","segment":{"start":0,"end":1,"text":"界"},"group":"active","authorization":"explicit"}"#;
        let decoded: Work = decode_json(legacy).unwrap();
        assert_eq!(decoded.attempts, 0);
        assert_eq!(decoded.state, "");
        assert!(decoded.next_at.is_none() && decoded.error.is_none());
        for state in ["", "pending", "running", "failed", "future-provider-state"] {
            let mut work = decoded.clone();
            work.state = state.into();
            let encoded = encode_json(&work).unwrap();
            let reread: Work = decode_json(&encoded).unwrap();
            assert_eq!(reread.state, state);
            assert_eq!(
                serde_json::from_str::<Value>(&encoded).unwrap()["state"],
                state
            );
        }
    }
    #[test]
    fn typed_updates_do_not_resurrect_removed_inputs_and_rollback_stays_atomic() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        let input = IndexKeys::input("job");
        let work = work();
        assert_eq!(update_json_meta(&db, &input, &work).unwrap(), 0);
        assert!(json_meta::<Work>(&db, &input).unwrap().is_none());
        {
            let tx = db.transaction().unwrap();
            set_json_meta(&tx, &input, &work).unwrap();
            let mut changed = work.clone();
            changed.state = "running".into();
            assert_eq!(update_json_meta(&tx, &input, &changed).unwrap(), 1);
            assert_eq!(
                json_meta::<Work>(&tx, &input).unwrap().unwrap().state,
                "running"
            );
        }
        assert!(json_meta::<Work>(&db, &input).unwrap().is_none());
    }
}
