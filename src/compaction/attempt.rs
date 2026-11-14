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

/// Replaces W2/W4 bodies while keeping the stage facts of the same token.
pub(crate) const FINISH_SQL: &str = "UPDATE model_attempt SET error_json=json_set(COALESCE(error_json,'{}'),'$.automatic_trim_attempt',json_set(json(?1),'$.token',?3,'$.compaction_outcome',json_extract(error_json,'$.automatic_trim_attempt.compaction_outcome'),'$.committed_stages',json(COALESCE(CASE WHEN json_extract(error_json,'$.automatic_trim_attempt.token')=?3 THEN json_extract(error_json,'$.automatic_trim_attempt.committed_stages') END,'{}')))) WHERE id=?2 AND (error_json IS NULL OR json_valid(error_json)) AND state IN ('preparing','inflight','failed','completed') AND json_extract(error_json,'$.automatic_trim_attempt.token')=?3";

/// Whether beginning an attempt observes the first-known-A lifecycle boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ABoundaryObservation {
    Observe,
    Unobserved,
}

/// Writes the provisional `unknown` record inside the witness transaction.
pub(crate) fn begin_tx(
    tx: &rusqlite::Transaction<'_>,
    attempt: &str,
    token: &str,
    reason: &str,
    a_boundary: ABoundaryObservation,
    trigger: &Value,
    target_plan: &Value,
) -> Result<(), StorageError> {
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
    let body = json!({"outcome":"unknown","provisional":true,"token":token,"reason":reason,
        "trigger":trigger,"target_plan":target_plan,"committed_stages":{}})
    .to_string();
    tx.execute(
        "UPDATE model_attempt SET error_json=json_set(COALESCE(error_json,'{}'),'$.automatic_trim_attempt',json(?1)) WHERE id=?2 AND (error_json IS NULL OR json_valid(error_json))",
        params![body, attempt],
    )?;
    Ok(())
}

pub(crate) fn finish_tx(
    tx: &rusqlite::Transaction<'_>,
    body: &str,
    attempt: &str,
    token: &str,
) -> Result<bool, StorageError> {
    let row:Option<(String,Option<String>)>=tx.query_row("SELECT t.thread_id,json_extract(a.error_json,'$.automatic_trim_attempt.trigger.a_boundary') FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE a.id=?1 AND json_extract(a.error_json,'$.automatic_trim_attempt.token')=?2 AND json_extract(a.error_json,'$.automatic_trim_attempt.provisional')=1",params![attempt,token],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((thread, boundary)) = row else {
        return Ok(false);
    };
    let changed = tx.execute(FINISH_SQL, params![body, attempt, token])?;
    if changed == 1
        && let Some(boundary) = boundary
    {
        let expected =
            serde_json::from_str(&boundary).map_err(|e| StorageError::Schema(e.to_string()))?;
        super::a_state::reset_tx(tx, &thread, expected, token)?;
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_finish_with_an_older_token_cannot_replace_a_newer_record() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE model_attempt(id TEXT PRIMARY KEY, error_json TEXT,state TEXT DEFAULT 'inflight')")
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
        // A finish cannot resurrect a cleared observation.
        assert_eq!(finish(&db, "first", "new"), 0);
        db.execute(
            "UPDATE model_attempt SET error_json=?1",
            [json!({"automatic_trim_attempt":{"token":"new"}}).to_string()],
        )
        .unwrap();
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
        // Interruption/recovery clears the token, including a captured finish.
        db.execute("UPDATE model_attempt SET error_json='{}'", [])
            .unwrap();
        assert_eq!(finish(&db, "late", "new"), 0);
        db.execute(
            "UPDATE model_attempt SET state='completed',error_json=?1",
            [json!({"automatic_trim_attempt":{"token":"new"}}).to_string()],
        )
        .unwrap();
        assert_eq!(finish(&db, "qualified_completed", "new"), 1);
        db.execute("UPDATE model_attempt SET error_json='{}'", [])
            .unwrap();
        assert_eq!(finish(&db, "cleared_completed", "new"), 0);
    }

    #[test]
    fn unobserved_a_boundary_skips_corrupt_state_and_finish_does_not_reset_it() {
        let db = crate::control::test_support::terminal_attempt_fixture("trial", "completed");
        db.execute(
            "INSERT INTO app_meta(key,value) VALUES('compact.a_state.t','{')",
            [],
        )
        .unwrap();
        let trigger = json!({"input_tokens":200,"a_input_tokens":null});
        {
            let tx = db.unchecked_transaction().unwrap();
            begin_tx(
                &tx,
                "trial",
                "off-token",
                "test",
                ABoundaryObservation::Unobserved,
                &trigger,
                &Value::Null,
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let begun: String = db
            .query_row(
                "SELECT error_json FROM model_attempt WHERE id='trial'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let begun: Value = serde_json::from_str(&begun).unwrap();
        assert!(
            begun["automatic_trim_attempt"]["trigger"]
                .get("a_boundary")
                .is_none()
        );
        assert!(begun["automatic_trim_attempt"]["trigger"]["a_input_tokens"].is_null());
        {
            let tx = db.unchecked_transaction().unwrap();
            assert!(
                finish_tx(
                    &tx,
                    &json!({"outcome":"adopted","trigger":trigger}).to_string(),
                    "trial",
                    "off-token",
                )
                .unwrap()
            );
            tx.commit().unwrap();
        }
        let (saved, raw_a): (String, String) = db.query_row(
            "SELECT a.error_json,m.value FROM model_attempt a CROSS JOIN app_meta m WHERE a.id='trial' AND m.key='compact.a_state.t'",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        let saved: Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved["automatic_trim_attempt"]["outcome"], "adopted");
        assert_eq!(raw_a, "{", "unobserved A bytes stay untouched");
    }

    #[test]
    fn observe_policy_still_rejects_corrupt_a_without_partial_attempt_write() {
        let db = crate::control::test_support::terminal_attempt_fixture("trial", "completed");
        db.execute(
            "INSERT INTO app_meta(key,value) VALUES('compact.a_state.t','{')",
            [],
        )
        .unwrap();
        let before: Option<String> = db
            .query_row(
                "SELECT error_json FROM model_attempt WHERE id='trial'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        {
            let tx = db.unchecked_transaction().unwrap();
            assert!(
                begin_tx(
                    &tx,
                    "trial",
                    "on-token",
                    "test",
                    ABoundaryObservation::Observe,
                    &json!({}),
                    &Value::Null,
                )
                .is_err()
            );
        }
        let after: Option<String> = db
            .query_row(
                "SELECT error_json FROM model_attempt WHERE id='trial'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let raw_a: String = db
            .query_row(
                "SELECT value FROM app_meta WHERE key='compact.a_state.t'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            after, before,
            "failed observe rolled back all attempt writes"
        );
        assert_eq!(raw_a, "{");
    }

    #[test]
    fn actual_terminal_cas_manual_order_duplicate_and_rollback() {
        for manual_first in [true, false] {
            let mut db =
                crate::control::test_support::terminal_attempt_fixture("trial", "completed");
            let start = super::super::a_state::scope_tx(&db, "t", "c", "m")
                .unwrap()
                .token();
            super::super::a_state::complete_report_tx(
                &db,
                "t",
                "c",
                "m",
                "old",
                Some(start),
                Some(111),
            )
            .unwrap();
            let trigger = json!({"a_boundary_token":start});
            {
                let tx = db.transaction().unwrap();
                begin_tx(
                    &tx,
                    "trial",
                    "token",
                    "test",
                    ABoundaryObservation::Observe,
                    &trigger,
                    &json!({}),
                )
                .unwrap();
                tx.commit().unwrap();
            }
            let reset = |db: &rusqlite::Connection| {
                let state = super::super::a_state::read(db, "t").unwrap().unwrap();
                super::super::a_state::reset_tx(db, "t", state.token(), "manual").unwrap();
            };
            if manual_first {
                reset(&db);
            }
            {
                let tx = db.transaction().unwrap();
                assert!(
                    finish_tx(
                        &tx,
                        &json!({"outcome":"noop","trigger":trigger}).to_string(),
                        "trial",
                        "token"
                    )
                    .unwrap()
                );
                tx.commit().unwrap();
            }
            if !manual_first {
                reset(&db);
            }
            let after_resets = super::super::a_state::read(&db, "t").unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(after_resets.token()).unwrap()["boundary"],
                if manual_first { 1 } else { 2 },
                "both commit orders have explicit expected reset counts"
            );
            assert_eq!(
                after_resets.reading(),
                super::super::a_state::Reading::AwaitingFirst,
                "successful reset clears the previous first"
            );
            let generation = super::super::a_state::begin_report_tx(&db, "t", "c", "m").unwrap();
            super::super::a_state::complete_report_tx(
                &db,
                "t",
                "c",
                "m",
                "report",
                Some(generation),
                Some(321),
            )
            .unwrap();
            let before = super::super::a_state::read(&db, "t").unwrap().unwrap();
            {
                let tx = db.transaction().unwrap();
                assert!(
                    !finish_tx(
                        &tx,
                        &json!({"outcome":"noop","trigger":trigger}).to_string(),
                        "trial",
                        "token"
                    )
                    .unwrap()
                );
                tx.commit().unwrap();
            }
            assert_eq!(
                super::super::a_state::read(&db, "t").unwrap().unwrap(),
                before
            );
            {
                let tx = db.transaction().unwrap();
                begin_tx(
                    &tx,
                    "trial",
                    "rollback",
                    "test",
                    ABoundaryObservation::Observe,
                    &json!({"a_boundary_token":before.token()}),
                    &json!({}),
                )
                .unwrap();
                assert!(
                    finish_tx(
                        &tx,
                        &json!({"outcome":"failed","trigger":{"a_boundary_token":before.token()}})
                            .to_string(),
                        "trial",
                        "rollback"
                    )
                    .unwrap()
                );
            }
            assert_eq!(
                super::super::a_state::read(&db, "t").unwrap().unwrap(),
                before
            );
        }
    }
}
