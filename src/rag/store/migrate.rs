//! Startup-only upgrades; read-only operations never interpret an older schema.
use super::{BlockRanges, Connection, HOT_QUERY_INDEXES, Result, err};

pub(super) fn run(db: &Connection, version: i64) -> Result<()> {
    if version == 4 {
        return Ok(());
    }
    let tx = db.unchecked_transaction()?;
    if version == 1 {
        tx.execute_batch(
            "ALTER TABLE sources ADD COLUMN exclude_json TEXT NOT NULL DEFAULT '[]';",
        )?;
    }
    let has_text: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('chunks') WHERE name='text')",
        [],
        |row| row.get(0),
    )?;
    if has_text {
        let revisions = {
            let mut q = tx.prepare("SELECT DISTINCT revision_id FROM chunks")?;
            q.query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for revision in revisions {
            let blocks = BlockRanges::load(&tx, &revision)?;
            let mut q = tx.prepare("SELECT start,end,text FROM chunks WHERE revision_id=?1")?;
            let rows = q.query_map([&revision], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (start, end, saved) = row?;
                let actual = blocks.get(start, end);
                if actual != Some(saved.as_str()) {
                    return Err(err(
                        "index_incompatible",
                        "Cannot migrate: a saved chunk differs from its block range",
                    ));
                }
            }
        }
    }
    tx.execute_batch("CREATE TABLE chunks_v4(id TEXT PRIMARY KEY,revision_id TEXT NOT NULL REFERENCES revisions(id),ordinal INTEGER NOT NULL,start INTEGER NOT NULL,end INTEGER NOT NULL,hash TEXT NOT NULL,location TEXT NOT NULL,UNIQUE(revision_id,ordinal));
        INSERT INTO chunks_v4 SELECT id,revision_id,ordinal,start,end,hash,location FROM chunks;
        DROP TABLE chunks;
        ALTER TABLE chunks_v4 RENAME TO chunks;")?;
    tx.execute_batch(HOT_QUERY_INDEXES)?;
    tx.pragma_update(None, "user_version", 4)?;
    tx.commit()?;
    Ok(())
}
