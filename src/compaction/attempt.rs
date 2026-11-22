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
}

/// GC-only stage token; unlike ordinary automatic stages it always carries
/// the original parent receipt and its qualified target compare-and-swap.
#[derive(Clone, Debug)]
pub(crate) struct GcStage {
    pub(crate) attempt_id: String,
    pub(crate) token: String,
    pub(crate) thread_id: String,
    pub(crate) stage: &'static str,
}

pub(crate) fn gc_matches(db: &rusqlite::Connection, stage: &GcStage) -> Result<bool, StorageError> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM automatic_compaction c JOIN turn t ON t.id=c.turn_id WHERE c.attempt_id=?1 AND c.token=?2 AND t.thread_id=?3)", params![stage.attempt_id, stage.token, stage.thread_id], |r| r.get(0))?)
}

#[derive(Clone, Debug)]
pub(crate) struct GcStageFact {
    pub(crate) selection_revision: i64,
    pub(crate) reasoning_epoch: i64,
    pub(crate) summary_node_id: String,
    pub(crate) source_nodes: u64,
    pub(crate) spilled_groups: u64,
    pub(crate) drop_all_reasoning: bool,
    pub(crate) captured_reasoning_epoch: i64,
    pub(crate) source_characters: Option<u64>,
    pub(crate) summary_characters: Option<u64>,
    pub(crate) summary_limit: Option<u64>,
    pub(crate) model_requests: Option<u64>,
    pub(crate) summary_usage: Option<super::SummaryUsage>,
}
impl GcStageFact {
    fn value(&self, stage: &GcStage) -> Value {
        json!({"stage":stage.stage,"selection_revision":self.selection_revision,"reasoning_epoch":self.reasoning_epoch,
            "summary_node_id":self.summary_node_id,"source_nodes":self.source_nodes,"spilled_groups":self.spilled_groups,
            "drop_all_reasoning":self.drop_all_reasoning,"captured_reasoning_epoch":self.captured_reasoning_epoch,
            "source_characters":self.source_characters,"summary_characters":self.summary_characters,
            "summary_limit":self.summary_limit,"model_requests":self.model_requests,"summary_usage":self.summary_usage})
    }
}

pub(crate) fn record_gc_stage_tx(
    tx: &rusqlite::Transaction<'_>,
    stage: &GcStage,
    fact: &GcStageFact,
) -> Result<(), StorageError> {
    if fact.summary_node_id.is_empty() || fact.summary_node_id.len() > 128 || fact.source_nodes == 0
    {
        return Err(StorageError::InvalidInput(
            "invalid bounded GC stage fact".into(),
        ));
    }
    write_stage(
        tx,
        &stage.attempt_id,
        &stage.token,
        stage.stage,
        &fact.value(stage),
    )
}

fn write_stage(
    tx: &rusqlite::Transaction<'_>,
    attempt: &str,
    token: &str,
    stage: &str,
    fact: &Value,
) -> Result<(), StorageError> {
    let column = match stage {
        "closed_spill" => "closed_spill",
        "active_spill" => "active_spill",
        "closed_summary" => "closed_summary",
        "active_summary" => "active_summary",
        _ => {
            return Err(StorageError::InvalidInput(
                "unknown automatic compaction stage".into(),
            ));
        }
    };
    let changed = tx.execute(
        &format!("UPDATE automatic_compaction SET {column}=?3 WHERE attempt_id=?1 AND token=?2"),
        params![attempt, token, fact.to_string()],
    )?;
    if changed != 1 {
        return Err(StorageError::InvalidInput(
            "automatic compaction identity changed".into(),
        ));
    }
    Ok(())
}

/// Whether beginning an attempt observes the first-known-A lifecycle boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ABoundaryObservation {
    Observe,
    Unobserved,
}

/// Consumes the triggering Conversation attempt exactly once.
pub(crate) fn begin_tx(
    tx: &rusqlite::Transaction<'_>,
    attempt: &str,
    token: &str,
    reason: &str,
    a_boundary: ABoundaryObservation,
    trigger: &Value,
) -> Result<bool, StorageError> {
    let mut trigger = trigger.clone();
    if a_boundary == ABoundaryObservation::Observe {
        let scope: Option<(String, String, String)> = tx
            .query_row(
                "SELECT t.thread_id,a.profile_id,a.model_id FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE a.id=?1",
                [attempt],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((thread, connection, model)) = scope {
            let state = super::a_state::scope_tx(tx, &thread, &connection, &model)?;
            trigger["a_boundary"] = serde_json::to_value(state.token())
                .map_err(|e| StorageError::Schema(e.to_string()))?;
        }
    }
    Ok(tx.execute(
        "INSERT INTO automatic_compaction(attempt_id,turn_id,attempt_started_at,token,reason,trigger_json) SELECT id,turn_id,started_at,?2,?3,?4 FROM model_attempt WHERE id=?1 AND kind='conversation' ON CONFLICT(attempt_id) DO NOTHING",
        params![attempt,token,reason,trigger.to_string()],
    )? == 1)
}

pub(crate) fn finish_tx(
    tx: &rusqlite::Transaction<'_>,
    attempt: &str,
    token: &str,
    outcome: &str,
    result: Option<&Value>,
) -> Result<bool, StorageError> {
    let changed=tx.execute("UPDATE automatic_compaction SET outcome=?3,result_json=?4,ended_at=?5 WHERE attempt_id=?1 AND token=?2",params![attempt,token,outcome,result.map(Value::to_string),chrono::Utc::now().to_rfc3339()])?;
    if changed == 1 {
        let (thread, boundary): (String, Option<String>) = tx.query_row("SELECT t.thread_id,json_extract(c.trigger_json,'$.a_boundary') FROM automatic_compaction c JOIN turn t ON t.id=c.turn_id WHERE c.attempt_id=?1",[attempt],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if let Some(boundary) = boundary {
            let expected =
                serde_json::from_str(&boundary).map_err(|e| StorageError::Schema(e.to_string()))?;
            super::a_state::reset_tx(tx, &thread, expected, token)?;
        }
    }
    Ok(changed == 1)
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

        "summary_node_id":result.summary_node_id,"removed_characters":measure.removed_characters,
        "source_characters":result.source_characters,"summary_characters":result.summary_characters,
        "summary_limit":result.summary_limit,"model_requests":result.model_requests,
        "summary_usage":result.summary_usage});
    write_stage(tx, &stage.attempt_id, &stage.token, stage.stage, &fact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_owns_only_terminal_facts_and_requires_original_token() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(r#"CREATE TABLE turn(id TEXT, thread_id TEXT);
            INSERT INTO turn VALUES('u','t');
            CREATE TABLE automatic_compaction(attempt_id TEXT,turn_id TEXT,token TEXT,trigger_json TEXT,closed_spill TEXT,failure_json TEXT,outcome TEXT,result_json TEXT,ended_at TEXT);
            INSERT INTO automatic_compaction VALUES('a','u','new','{}','{"spilled_groups":1}','{"elapsed_ms":3}',NULL,NULL,NULL);"#).unwrap();
        let tx = db.transaction().unwrap();
        assert!(!finish_tx(&tx, "a", "old", "adopted", None).unwrap());
        assert!(finish_tx(&tx, "a", "new", "failed", None).unwrap());
        assert!(tx.query_row("SELECT outcome='failed' AND json_extract(closed_spill,'$.spilled_groups')=1 AND json_extract(failure_json,'$.elapsed_ms')=3 AND ended_at IS NOT NULL FROM automatic_compaction",[],|r|r.get::<_,bool>(0)).unwrap());
        assert!(!finish_tx(&tx, "missing", "new", "failed", None).unwrap());
    }
}
