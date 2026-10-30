//! Explicit, transactional collection of retained inactive vector spaces.
use super::*;

pub(crate) fn collect_inactive_spaces(db: &mut Connection) -> Result<u64, StorageError> {
    let tx = db.transaction()?;
    let spaces = tx
        .prepare(
            "SELECT s.id,s.vec_table_name FROM index_space s
         WHERE s.state='inactive'
         AND NOT EXISTS(SELECT 1 FROM app_meta WHERE key='index.active_space' AND value=s.id)
         AND NOT EXISTS(SELECT 1 FROM embedding_job WHERE space_id=s.id AND state='running')
         AND NOT EXISTS(SELECT 1 FROM app_meta g JOIN app_meta r
             ON g.key='index.group.'||substr(r.key,length('index.rebuild.')+1)||'.'||r.value
             WHERE r.key GLOB 'index.rebuild.*' AND g.value=s.id)
         ORDER BY s.id LIMIT 32",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (space, table) in &spaces {
        valid_table(table)?;
        tx.execute_batch(&format!("DROP TABLE {table}"))?;
        tx.execute("DELETE FROM app_meta WHERE key IN (SELECT 'index.preview.'||id FROM embedding_chunk WHERE space_id=?1)", [space])?;
        tx.execute("DELETE FROM embedding_chunk WHERE space_id=?1", [space])?;
        tx.execute("DELETE FROM app_meta WHERE key IN (SELECT 'index.input.'||id FROM embedding_job WHERE space_id=?1)", [space])?;
        tx.execute("DELETE FROM embedding_job WHERE space_id=?1", [space])?;
        tx.execute("DELETE FROM app_meta WHERE key=?1 OR key=?2 OR (key GLOB 'index.group.*' AND value=?3)",
            params![IndexKeys::query_profile(space), dimensions::space_key(space), space])?;
        tx.execute("DELETE FROM index_space WHERE id=?1", [space])?;
    }
    tx.commit()?;
    Ok(spaces.len() as u64)
}
