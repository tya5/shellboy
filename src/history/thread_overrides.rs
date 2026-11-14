//! Raw per-Thread overrides shared by branch and child creation.
use rusqlite::{Connection, params};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ThreadOverrides {
    auto_compact_at: Option<i64>,
    target_percent: Option<i64>,
    summary_max_chars: Option<i64>,
    max_tokens: Option<i64>,
    spill: Option<bool>,
}

impl ThreadOverrides {
    pub(crate) fn inherit(db: &Connection, parent: Option<&str>) -> rusqlite::Result<Self> {
        let Some(parent) = parent else {
            return Ok(Self::default());
        };
        db.query_row(
            "SELECT auto_compact_at,auto_compact_target_percent,summary_max_chars,max_tokens,spill_override FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [parent],
            |row| Ok(Self {
                auto_compact_at: row.get(0)?,
                target_percent: row.get(1)?,
                summary_max_chars: row.get(2)?,
                max_tokens: row.get(3)?,
                spill: crate::storage::decode_spill_override(row.get_ref(4)?, 4)?,
            }),
        )
    }

    pub(crate) fn apply(self, db: &Connection, thread: &str) -> rusqlite::Result<()> {
        // Copy NULL itself, not the parent's currently resolved connection default.
        let changed = db.execute(
            "UPDATE thread SET auto_compact_at=?2,auto_compact_target_percent=?3,summary_max_chars=?4,max_tokens=?5,spill_override=?6 WHERE id=?1 AND deleted_at IS NULL",
            params![thread,self.auto_compact_at,self.target_percent,self.summary_max_chars,self.max_tokens,self.spill.map(i64::from)],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }
}
