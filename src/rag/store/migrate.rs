//! Startup-only upgrades; read-only operations never interpret an older schema.
use super::{BlockRanges, Connection, HOT_QUERY_INDEXES, Result, SOURCE_HASH_SCHEMA, err};

pub(super) fn run(db: &Connection, version: i64) -> Result<()> {
    if version == 7 {
        return Ok(());
    }
    let tx = db.unchecked_transaction()?;
    if version == 1 {
        tx.execute_batch(
            "ALTER TABLE sources ADD COLUMN exclude_json TEXT NOT NULL DEFAULT '[]';",
        )?;
    }
    if version < 4 {
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
    }
    if version < 5 {
        tx.execute_batch(SOURCE_HASH_SCHEMA)?;
        tx.execute_batch("INSERT OR IGNORE INTO source_chunk_hashes SELECT d.source_id,c.hash FROM chunks c JOIN revisions r ON r.id=c.revision_id JOIN documents d ON d.id=r.document_id;")?;
        tx.execute_batch(HOT_QUERY_INDEXES)?;
    }
    if version < 6 {
        tx.execute_batch(include_str!("next_retire_builtin_docs.sql"))?;
    }
    tx.execute_batch("ALTER TABLE revisions ADD COLUMN reference TEXT NOT NULL DEFAULT ''; ALTER TABLE revisions ADD COLUMN reader_args TEXT NOT NULL DEFAULT '[]';")?;
    {
        let mut query = tx.prepare(
            "SELECT r.id,d.source_id,d.key FROM revisions r JOIN documents d ON d.id=r.document_id",
        )?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let source: String = row.get(1)?;
            let key: String = row.get(2)?;
            let reference = super::records::file_ref(&source, &key);
            let args = serde_json::json!(["rag", "get", "--ref", reference]).to_string();
            tx.execute(
                "UPDATE revisions SET reference=?2,reader_args=?3 WHERE id=?1",
                rusqlite::params![id, reference, args],
            )?;
        }
    }
    tx.pragma_update(None, "user_version", 7)?;
    tx.commit()?;
    Ok(())
}
