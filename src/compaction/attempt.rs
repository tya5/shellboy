//! #226: recoverable automatic attempt record. Each automatic stage appends a
//! small, content-free fact to the triggering attempt's provisional record in
//! the same transaction that adopts its replacement, so the record is either
//! final or `unknown` with the exact set of committed stages.
use super::*;

/// Identifies the provisional record a stage writes into. Manual and overflow
/// compaction pass `None` and are unchanged.
#[derive(Clone, Debug)]
pub(crate) struct AutomaticStage {
    pub(crate) attempt_id: String,
    pub(crate) token: String,
    /// `closed_spill`, `closed_summary`, `active_spill` or `active_summary`.
    pub(crate) stage: &'static str,
    /// Calibrated attempts: the pending-target key and its provisional value.
    pub(crate) target: Option<(String, String)>,
}

/// Replaces W2/W4 bodies while keeping the stage facts of the same token.
pub(crate) const FINISH_SQL: &str = "UPDATE model_attempt SET error_json=json_set(COALESCE(error_json,'{}'),'$.automatic_trim_attempt',json_set(json(?1),'$.token',?3,'$.committed_stages',json(COALESCE(CASE WHEN json_extract(error_json,'$.automatic_trim_attempt.token')=?3 THEN json_extract(error_json,'$.automatic_trim_attempt.committed_stages') END,'{}')))) WHERE id=?2 AND (error_json IS NULL OR json_valid(error_json)) AND COALESCE(json_extract(error_json,'$.automatic_trim_attempt.token'),?3)=?3";

/// Writes the provisional `unknown` record inside the witness transaction.
pub(crate) fn begin_tx(
    tx: &rusqlite::Transaction<'_>,
    attempt: &str,
    token: &str,
    reason: &str,
    trigger: &Value,
    target_plan: &Value,
) -> Result<(), StorageError> {
    let body = json!({"outcome":"unknown","provisional":true,"token":token,"reason":reason,
        "trigger":trigger,"target_plan":target_plan,"committed_stages":{}})
    .to_string();
    tx.execute(
        "UPDATE model_attempt SET error_json=json_set(COALESCE(error_json,'{}'),'$.automatic_trim_attempt',json(?1)) WHERE id=?2 AND (error_json IS NULL OR json_valid(error_json))",
        params![body, attempt],
    )?;
    Ok(())
}

/// Fixed-size measurements of one adopted stage. `removed_characters` is the
/// exact replay-projection difference of the replaced items under the
/// adoption snapshot's epoch; it excludes the effect of an epoch advance on
/// other reasoning, which only the driver's re-measurement establishes.
#[derive(Default)]
pub(super) struct StageMeasure {
    pub(super) removed_characters: Option<u64>,
}

/// Called inside the stage's adoption transaction, after the path and epoch
/// are written. Any error rolls the adoption back with it.
pub(super) fn record_tx(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    stage: &AutomaticStage,
    result: &CompactResult,
    measure: &StageMeasure,
) -> Result<(), StorageError> {
    let (revision, epoch): (i64, i64) = tx.query_row(
        "SELECT selection_revision,reasoning_epoch FROM thread WHERE id=?1",
        [thread],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let fact = json!({"stage":stage.stage,"selection_revision":revision,"reasoning_epoch":epoch,
        "spilled_groups":result.spilled_groups,"source_nodes":result.source_nodes,
        "created_parts":result.created_parts,"reused_parts":result.reused_parts,
        "summary_node_id":result.summary_node_id,"removed_characters":measure.removed_characters,
        "source_characters":result.source_characters,"summary_characters":result.summary_characters,
        "summary_limit":result.summary_limit,"model_requests":result.model_requests,
        "summary_usage":result.summary_usage})
    .to_string();
    tx.execute(
        "UPDATE model_attempt SET error_json=json_set(error_json,?4,json(?3)) WHERE id=?1 AND json_valid(error_json) AND json_extract(error_json,'$.automatic_trim_attempt.token')=?2",
        params![
            stage.attempt_id,
            stage.token,
            fact,
            format!("$.automatic_trim_attempt.committed_stages.{}", stage.stage)
        ],
    )?;
    if let Some((key, value)) = &stage.target {
        tx.execute(
            "INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_finish_with_an_older_token_cannot_replace_a_newer_record() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE model_attempt(id TEXT PRIMARY KEY, error_json TEXT)")
            .unwrap();
        db.execute("INSERT INTO model_attempt(id) VALUES('a')", [])
            .unwrap();
        let read = |db: &rusqlite::Connection| -> Value {
            let text: String = db
                .query_row("SELECT error_json FROM model_attempt", [], |r| r.get(0))
                .unwrap();
            serde_json::from_str(&text).unwrap()
        };
        let finish = |db: &rusqlite::Connection, outcome: &str, token: &str| {
            db.execute(
                FINISH_SQL,
                params![json!({"outcome":outcome}).to_string(), "a", token],
            )
            .unwrap()
        };
        // No record yet: the first finish writes.
        assert_eq!(finish(&db, "first", "new"), 1);
        assert_eq!(read(&db)["automatic_trim_attempt"]["token"], "new");
        // A late write from an older attempt token changes nothing.
        assert_eq!(finish(&db, "stale", "old"), 0);
        let record = read(&db);
        assert_eq!(record["automatic_trim_attempt"]["outcome"], "first");
        assert_eq!(record["automatic_trim_attempt"]["token"], "new");
        // The same token may still replace its own record.
        assert_eq!(finish(&db, "second", "new"), 1);
        assert_eq!(read(&db)["automatic_trim_attempt"]["outcome"], "second");
    }
}
