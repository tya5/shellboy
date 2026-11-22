//! Project the owner's next reset from parked Turns; admission stays in run.retry.
use chrono::{DateTime, Datelike, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

const CONSUMED: &str = "consumed_usage_limit_reset";

pub(super) fn reset(reason: &Value) -> Option<DateTime<Utc>> {
    let limit = &reason["details"]["usage_limit"];
    if limit["kind"].as_str()? != "usage_limit_reached" {
        return None;
    }
    let at = DateTime::from_timestamp(limit["resets_at"].as_i64()?, 0)?;
    (at.year() <= 9999 && at.timestamp() > 0).then_some(at)
}

pub(super) fn consumed(reason: &Value) -> Option<i64> {
    reason[CONSUMED].as_i64()
}

pub(super) fn consumed_only(at: i64) -> String {
    serde_json::json!({CONSUMED: at}).to_string()
}

pub(super) fn mark_consumed(reason: &mut Value, at: i64) {
    reason[CONSUMED] = consumed(reason).map_or(at, |old| old.max(at)).into();
}

fn eligible_reset(reason: &Value) -> Option<DateTime<Utc>> {
    let at = reset(reason)?;
    if let Some(previous) = consumed(reason) {
        let failed =
            DateTime::parse_from_rfc3339(reason["details"]["last_failure_at"].as_str()?).ok()?;
        if at.timestamp() <= previous || at <= failed {
            return None;
        }
    }
    Some(at)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Candidate {
    pub thread: String,
    pub run: String,
    pub turn: String,
    pub generation: i64,
    pub at: DateTime<Utc>,
}

fn parked(
    db: &Connection,
    run: Option<&str>,
    mut visit: impl FnMut(Candidate),
) -> rusqlite::Result<()> {
    let mut rows = db.prepare(
        "SELECT u.thread_id,u.run_id,u.id,u.generation,u.recovery_reason_json
         FROM turn u INDEXED BY one_open_turn JOIN run r ON r.id=u.run_id
         WHERE u.state IN ('preparing','running','waiting','recovery_wait')
           AND u.state='recovery_wait' AND r.state='live' AND u.interrupt_requested_at IS NULL
           AND u.recovery_reason_json IS NOT NULL AND (?1 IS NULL OR r.id=?1)",
    )?;
    let mut rows = rows.query([run])?;
    while let Some(row) = rows.next()? {
        let reason: String = row.get(4)?;
        if let Some(at) = serde_json::from_str(&reason)
            .ok()
            .as_ref()
            .and_then(eligible_reset)
        {
            visit(Candidate {
                thread: row.get(0)?,
                run: row.get(1)?,
                turn: row.get(2)?,
                generation: row.get(3)?,
                at,
            });
        }
    }
    Ok(())
}

pub(super) fn next(db: &Connection) -> rusqlite::Result<Option<Candidate>> {
    let mut next: Option<Candidate> = None;
    parked(db, None, |candidate| {
        if next
            .as_ref()
            .is_none_or(|old| (&candidate.at, &candidate.run) < (&old.at, &old.run))
        {
            next = Some(candidate);
        }
    })?;
    Ok(next)
}

pub(super) fn preserved_runs(db: &Connection, enabled: bool) -> rusqlite::Result<String> {
    let mut runs = Vec::new();
    if enabled {
        parked(db, None, |candidate| runs.push(candidate.run))?;
    }
    Ok(serde_json::to_string(&runs).expect("Run IDs serialize"))
}

pub(super) fn preserve_run(db: &Connection, run: &str) -> rusqlite::Result<bool> {
    let mut preserve = false;
    parked(db, Some(run), |_| preserve = true)?;
    Ok(preserve)
}

pub(super) fn retained_reset(db: &Connection, turn: &str) -> rusqlite::Result<Option<i64>> {
    db.query_row(
        "SELECT json_extract(recovery_reason_json,'$.consumed_usage_limit_reset') FROM turn WHERE id=?1",
        [turn], |r| r.get(0),
    ).optional().map(Option::flatten)
}

pub(super) fn public_reason(reason: Option<String>) -> Option<String> {
    reason.filter(|text| {
        !serde_json::from_str::<Value>(text)
            .is_ok_and(|value| crate::context::is_consumed_reset_only(&value))
    })
}

pub(super) enum Step {
    Waiting(Option<DateTime<Utc>>),
    Admitted(String),
}

pub(super) fn step(
    tx: &rusqlite::Transaction<'_>,
    now: DateTime<Utc>,
) -> Result<Step, crate::storage::StorageError> {
    // Select and recheck the saved reason, generation and Run/Turn states at
    // the admission cut. A deadline is a wake-up hint, not retry authority.
    let Some(candidate) = next(tx)? else {
        return Ok(Step::Waiting(None));
    };
    if candidate.at > now {
        return Ok(Step::Waiting(Some(candidate.at)));
    }
    super::db::resume_run_tx(
        tx,
        (
            &candidate.thread,
            &candidate.run,
            &candidate.turn,
            candidate.generation,
        ),
        serde_json::json!({"kind":"owner"}),
        &now.to_rfc3339(),
    )?;
    Ok(Step::Admitted(candidate.thread))
}

pub(super) async fn drive(owner: &super::Control) -> Result<Step, crate::storage::StorageError> {
    if !owner.store.config.recovery.auto_retry_usage_limit {
        return Ok(Step::Waiting(None));
    }
    let stop = owner.stop.clone();
    owner
        .store
        .database
        .call(move |db| {
            if stop.is_cancelled() {
                return Ok(Step::Waiting(None));
            }
            let tx = db.transaction()?;
            let result = step(&tx, Utc::now())?;
            tx.commit()?;
            Ok(result)
        })
        .await
}
