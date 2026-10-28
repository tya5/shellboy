//! Startup-only upgrades; read-only operations never interpret an older schema.
use super::{Connection, HOT_QUERY_INDEXES, Result};

pub(super) fn run(db: &Connection, version: i64) -> Result<()> {
    if version == 3 {
        return Ok(());
    }
    let tx = db.unchecked_transaction()?;
    if version == 1 {
        tx.execute_batch(
            "ALTER TABLE sources ADD COLUMN exclude_json TEXT NOT NULL DEFAULT '[]';",
        )?;
    }
    tx.execute_batch(HOT_QUERY_INDEXES)?;
    tx.pragma_update(None, "user_version", 3)?;
    tx.commit()?;
    Ok(())
}
