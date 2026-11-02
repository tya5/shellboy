//! Startup-only upgrades; read-only operations never interpret an older schema.
use super::{Connection, HOT_QUERY_INDEXES, Result, err};

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
        let invalid: bool = tx.query_row("WITH positioned AS (SELECT revision_id,text,COALESCE(SUM(length(CAST(text AS BLOB))+1) OVER (PARTITION BY revision_id ORDER BY ordinal ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING),0) AS start FROM blocks) SELECT EXISTS(SELECT 1 FROM chunks c WHERE NOT EXISTS(SELECT 1 FROM positioned b WHERE b.revision_id=c.revision_id AND c.start>=b.start AND c.end>=c.start AND c.end<=b.start+length(CAST(b.text AS BLOB)) AND substr(CAST(b.text AS BLOB),c.start-b.start+1,c.end-c.start)=CAST(c.text AS BLOB)))", [], |row| row.get(0))?;
        if invalid {
            return Err(err(
                "index_incompatible",
                "Cannot migrate: a saved chunk differs from its block range",
            ));
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
