//! Bounded deletion scope: the selected Thread and, with descendants, its
//! finite live subtree. Exact review and the deletion CAS stay in the
//! canonical implementation; this module never grants permission.
use crate::{
    protocol::{ApiError, ErrorCode, ThreadDeleteArgs},
    storage::StorageError,
};
use rusqlite::Connection;
use std::collections::BTreeSet;
pub(super) const THREAD_LIMIT: usize = 256;
// Deleted physical children consume admission before exclusion, but unlike
// Watch lineage they do NOT bridge traversal into their live grandchildren.
const SCOPE: &str = r"
WITH RECURSIVE root AS (SELECT id FROM thread WHERE id=?1 AND deleted_at IS NULL),
walk(id,parent,depth,path,cycle,deleted) AS (
 SELECT c.id,c.parent_thread_id,1,json_array(?1,c.id),c.id=?1,c.deleted_at IS NOT NULL
 FROM root CROSS JOIN thread c ON c.id=(SELECT min(id) FROM thread WHERE parent_thread_id=root.id)
 UNION ALL
 SELECT c.id,c.parent_thread_id,w.depth+1,json_insert(w.path,'$[#]',c.id),
   EXISTS(SELECT 1 FROM json_each(w.path) WHERE value=c.id),c.deleted_at IS NOT NULL
 FROM walk w CROSS JOIN thread c ON c.id=(SELECT min(id) FROM thread WHERE parent_thread_id=w.id)
 WHERE w.depth<8 AND NOT w.cycle AND NOT w.deleted
 UNION ALL
 SELECT c.id,c.parent_thread_id,w.depth,json_insert(json_remove(w.path,'$[#-1]'),'$[#]',c.id),
   EXISTS(SELECT 1 FROM json_each(json_remove(w.path,'$[#-1]')) WHERE value=c.id),c.deleted_at IS NOT NULL
 FROM walk w CROSS JOIN thread c ON c.id=(SELECT min(id) FROM thread WHERE parent_thread_id=w.parent AND id>w.id)
 ORDER BY 3,1 LIMIT 257
)
SELECT w.id,w.deleted,w.cycle,t.updated_at,
 w.depth=8 AND NOT w.deleted AND NOT w.cycle AND EXISTS(SELECT 1 FROM thread child WHERE child.parent_thread_id=w.id)
FROM walk w JOIN thread t ON t.id=w.id";
struct Member {
    id: String,
    deleted: bool,
    cycle: bool,
    depth_limited: bool,
}
#[derive(Debug, thiserror::Error)]
#[error("Deletion observation scalar exceeds512 bytes; inspect with the exact CLI preview")]
struct ScalarLimit;
fn scalar(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<String> {
    let value = row.get_ref(index)?;
    if let rusqlite::types::ValueRef::Text(bytes) = value
        && bytes.len() > 512
    {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(ScalarLimit),
        ));
    }
    let text = value.as_str().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(text.to_owned())
}
fn boundary_error(error: rusqlite::Error) -> StorageError {
    if let rusqlite::Error::FromSqlConversionFailure(_, _, source) = &error
        && source.downcast_ref::<ScalarLimit>().is_some()
    {
        return ApiError::new(ErrorCode::InputLimit, ScalarLimit.to_string()).into();
    }
    error.into()
}
fn read_members(db: &Connection, thread: &str) -> Result<Vec<Member>, StorageError> {
    let members = db
        .prepare(SCOPE)?
        .query_map([thread], |row| {
            Ok(Member {
                id: scalar(row, 0)?,
                deleted: row.get(1)?,
                cycle: row.get(2)?,
                depth_limited: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(boundary_error)?;
    Ok(members)
}

pub(super) fn selected_targets(
    db: &Connection,
    args: &ThreadDeleteArgs,
) -> Result<Option<Vec<String>>, StorageError> {
    if args.thread_id.len() > 256 {
        return Ok(None);
    }
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
        [&args.thread_id],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let mut selected = BTreeSet::from([args.thread_id.clone()]);
    if args.descendants {
        let members = read_members(db, &args.thread_id)?;
        if members.len() > THREAD_LIMIT || members.iter().any(|r| r.cycle || r.depth_limited) {
            return Ok(None);
        }
        selected.extend(members.into_iter().filter(|r| !r.deleted).map(|r| r.id));
    }
    Ok(Some(selected.into_iter().collect()))
}
