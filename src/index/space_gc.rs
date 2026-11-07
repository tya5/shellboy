//! Collection of index spaces. Automatic maintenance and `storage collect` take
//! only what holds no paid work: discarded partial candidates and inactive spaces
//! without vectors. A retained space goes only through `index remove`.
use super::*;
use crate::protocol::IndexRemoveArgs;

const SPACE_SELECT: &str = "SELECT s.id,s.vec_table_name FROM index_space s
         WHERE (s.state='discarded' OR (s.state='inactive' AND NOT EXISTS(SELECT 1 FROM embedding_chunk WHERE space_id=s.id)))
         AND NOT EXISTS(SELECT 1 FROM app_meta WHERE key='index.active_space' AND value=s.id)
         AND NOT EXISTS(SELECT 1 FROM embedding_job WHERE space_id=s.id AND state='running')
         AND NOT EXISTS(SELECT 1 FROM app_meta WHERE key='index.shrink.'||s.id)
         AND NOT EXISTS(SELECT 1 FROM app_meta g JOIN app_meta r
             ON g.key='index.group.'||substr(r.key,length('index.rebuild.')+1)||'.'||r.value
             WHERE r.key GLOB 'index.rebuild.*' AND g.value=s.id)
         ORDER BY s.id LIMIT 32";

pub(crate) fn collect_inactive_spaces(db: &mut Connection) -> Result<u64, StorageError> {
    let tx = db.transaction()?;
    let spaces = tx
        .prepare(SPACE_SELECT)?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (space, table) in &spaces {
        remove_space(&tx, space, table)?;
    }
    tx.commit()?;
    Ok(spaces.len() as u64)
}

fn remove_space(tx: &Transaction<'_>, space: &str, table: &str) -> Result<(), StorageError> {
    valid_table(table)?;
    tx.execute_batch(&format!("DROP TABLE {table}"))?;
    tx.execute("DELETE FROM app_meta WHERE key IN (SELECT 'index.preview.'||id FROM embedding_chunk WHERE space_id=?1)", [space])?;
    tx.execute("DELETE FROM embedding_chunk WHERE space_id=?1", [space])?;
    tx.execute("DELETE FROM app_meta WHERE key IN (SELECT 'index.input.'||id FROM embedding_job WHERE space_id=?1)", [space])?;
    tx.execute("DELETE FROM embedding_job WHERE space_id=?1", [space])?;
    tx.execute(
        "DELETE FROM app_meta WHERE key IN (?1,?2,?3) OR (key GLOB 'index.group.*' AND value=?4)",
        params![
            IndexKeys::query_profile(space),
            dimensions::space_key(space),
            IndexKeys::converted_from(space),
            space
        ],
    )?;
    tx.execute("DELETE FROM index_space WHERE id=?1", [space])?;
    Ok(())
}

/// The facts of removing one space. Read-only; `blocked_by` says why it cannot go.
fn removal_preview(db: &Connection, id: &str) -> Result<Value, StorageError> {
    let space = load_space(db, id)?.ok_or_else(|| {
        StorageError::from(err(ErrorCode::TargetUnavailable, "Unknown index space"))
    })?;
    let segments: i64 = db.query_row(
        "SELECT count(*) FROM embedding_chunk WHERE space_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let flag = |sql: &str| db.query_row(sql, [id], |row| row.get::<_, bool>(0));
    let blocked_by = if meta(db, IndexKeys::ACTIVE_SPACE)?.as_deref() == Some(id) {
        Some("it is the active index")
    } else if !matches!(space.state.as_str(), "inactive" | "discarded") {
        Some("it is not inactive")
    } else if flag(
        "SELECT EXISTS(SELECT 1 FROM embedding_job WHERE space_id=?1 AND state='running')",
    )? {
        Some("an embedding request for it is running")
    } else if shrink::involves(db, id)? {
        Some("a conversion is using it")
    } else if flag(
        "SELECT EXISTS(SELECT 1 FROM app_meta g JOIN app_meta r ON g.key='index.group.'||substr(r.key,length('index.rebuild.')+1)||'.'||r.value WHERE r.key GLOB 'index.rebuild.*' AND g.value=?1)",
    )? {
        Some("a rebuild is using it")
    } else {
        None
    };
    Ok(json!({
        "space_id":space.id,
        "connection":space.profile,
        "model":space.model,
        "dimension":space.dimension,
        "element_type":if space.int8_scale.is_some() {"int8"} else {"float32"},
        "state":space.state,
        "segments":segments,
        "vector_payload_bytes_freed":segments as u64 * space.dimension as u64 * space.element_bytes(),
        "removable":blocked_by.is_none(),
        "blocked_by":blocked_by,
        "effects":"Deletes this index and its vectors for good; conversation history is not touched. Recreating it means paid embedding of every segment. Freed pages are reused; the database file shrinks only with `storage vacuum`.",
    }))
}

impl IndexService {
    /// Remove one named inactive space. Nothing else ever deletes a retained one.
    pub async fn remove_space(&self, args: IndexRemoveArgs) -> Result<Value, ApiError> {
        crate::protocol::Command::IndexRemove(args.clone()).validate()?;
        let dry_run = args.dry_run;
        let result = self
            .database
            .call(move |db| {
                let mut preview = removal_preview(db, &args.space_id)?;
                preview["dry_run"] = json!(dry_run);
                if dry_run {
                    return Ok(preview);
                }
                if let Some(reason) = preview["blocked_by"].as_str() {
                    return Err(StorageError::from(err(
                        ErrorCode::InvalidState,
                        format!("Cannot remove this index: {reason}"),
                    )));
                }
                let tx = db.transaction()?;
                let table = load_space(&tx, &args.space_id)?
                    .ok_or_else(serde_error)?
                    .table;
                remove_space(&tx, &args.space_id, &table)?;
                tx.commit()?;
                preview["removed"] = json!(true);
                Ok(preview)
            })
            .await
            .map_err(db_error)?;
        if !dry_run {
            self.publish_saved(true);
        }
        Ok(result)
    }
}
