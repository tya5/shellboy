//! Revisit: an ordinary message sent with `--deliver complete`.
//!
//! While a Run is live the message waits as a held receipt without a Run or
//! Turn. It never outlives that Run: at normal completion every waiting message
//! is assigned, in send order, to one new Turn of that Run; when the Run stops,
//! is shut down or enters recovery wait, the waiting messages become ordinary
//! held input with that situation's hold reason, and the existing message
//! rules decide what happens next. No separate store, event kind or counter exists.
use crate::protocol::RevisitSummary;
use rusqlite::{Connection, Transaction, params};
use serde_json::json;

/// Hold reason while waiting, and the saved delivery-context reason afterwards.
pub(crate) const RUN_COMPLETION: &str = "run_completion";
/// Thread-level waiting messages; `{thread}` is an SQL expression.
pub(crate) const PENDING_SQL: &str = "EXISTS(SELECT 1 FROM receipt INDEXED BY receipt_target WHERE target_thread_id={thread} AND state='held' AND hold_reason='run_completion')";
/// Messages a Run received at completion; `{run}` is an SQL expression.
pub(crate) const DELIVERED_SQL: &str = "(SELECT count(*) FROM receipt INDEXED BY receipt_run_completion WHERE run_id={run} AND json_extract(result_json,'$.deliver')='complete' AND json_extract(result_json,'$.delivery_context.reason')='run_completion')";

pub(crate) fn summary_sql(thread: &str, run: &str) -> (String, String) {
    (
        PENDING_SQL.replace("{thread}", thread),
        DELIVERED_SQL.replace("{run}", run),
    )
}

/// Bounded display facts; None when the Run has neither.
pub(crate) fn summary(pending: bool, delivered: u64) -> Option<RevisitSummary> {
    (pending || delivered > 0).then_some(RevisitSummary {
        pending,
        consumed_count: delivered,
    })
}

pub(crate) fn summary_for_run(
    db: &Connection,
    run: &str,
) -> rusqlite::Result<Option<RevisitSummary>> {
    let (pending, delivered) = summary_sql("r.thread_id", "r.id");
    db.query_row(
        &format!("SELECT {pending},{delivered} FROM run r WHERE r.id=?1"),
        [run],
        |r| Ok(summary(r.get(0)?, r.get::<_, i64>(1)?.unsigned_abs())),
    )
    .or_else(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        error => Err(error),
    })
}

pub(super) fn has_pending(db: &Connection, thread: &str) -> rusqlite::Result<bool> {
    db.query_row(
        &format!("SELECT {}", PENDING_SQL.replace("{thread}", "?1")),
        [thread],
        |r| r.get(0),
    )
}

/// Called at the normal completion decision, in its transaction. Returns the
/// new Turn when waiting messages were assigned to it.
pub(super) fn release_tx(
    tx: &Transaction<'_>,
    thread: &str,
    run: &str,
    at: &str,
) -> rusqlite::Result<Option<String>> {
    let rows = tx
        .prepare("SELECT id,message_node_id FROM receipt WHERE target_thread_id=?1 AND state='held' AND hold_reason='run_completion' ORDER BY created_at,rowid")?
        .query_map([thread], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    let turn = super::state::create_turn_tx(tx, thread, run, at, at)?;
    let context =
        json!({"reason":RUN_COMPLETION,"run_id":run,"turn_id":turn,"admitted_at":at}).to_string();
    for (receipt, node) in rows {
        tx.execute("UPDATE receipt SET state='accepted',hold_reason=NULL,run_id=?2,turn_id=?3,updated_at=?4,result_json=json_set(COALESCE(result_json,'{}'),'$.assignment','new_turn','$.delivery_status','accepted','$.run_id',?2,'$.turn_id',?3,'$.delivery_context',json(?5)) WHERE id=?1",params![receipt,run,turn,at,context])?;
        tx.execute(
            "UPDATE node SET turn_id=?2 WHERE id=?1 AND turn_id IS NULL",
            params![node, turn],
        )?;
    }
    Ok(Some(turn))
}

/// The Run ended or stalled without completing: hand waiting messages to the
/// ordinary held-input rules. `turn` assigns them to a recovery-wait Turn.
pub(super) fn hand_off_tx(
    tx: &Transaction<'_>,
    thread: Option<&str>,
    hold_reason: &str,
    turn: Option<(&str, &str)>,
    at: &str,
) -> rusqlite::Result<()> {
    let waiting =
        "state='held' AND hold_reason='run_completion' AND (?1 IS NULL OR target_thread_id=?1)";
    if let Some((run, turn)) = turn {
        tx.execute(&format!("UPDATE node SET turn_id=?3 WHERE turn_id IS NULL AND id IN (SELECT message_node_id FROM receipt WHERE {waiting} AND ?2 IS NOT NULL)"),params![thread,run,turn])?;
        tx.execute(&format!("UPDATE receipt SET hold_reason=?4,run_id=?2,turn_id=?3,updated_at=?5,result_json=json_set(COALESCE(result_json,'{{}}'),'$.assignment','existing_turn','$.run_id',?2,'$.turn_id',?3) WHERE {waiting}"),params![thread,run,turn,hold_reason,at])?;
    } else {
        tx.execute(
            &format!("UPDATE receipt SET hold_reason=?2,updated_at=?3 WHERE {waiting}"),
            params![thread, hold_reason, at],
        )?;
    }
    Ok(())
}

/// Saved receipts as Revisit leaves them: `delivered` completion deliveries to
/// `run`, plus one waiting message when `pending`.
#[cfg(test)]
pub(crate) fn seed_fixture(
    db: &Connection,
    thread: &str,
    run: &str,
    delivered: u64,
    pending: bool,
) {
    for i in 0..delivered + u64::from(pending) {
        let id = format!("revisit-fixture-{run}-{i}");
        let waiting = i == delivered;
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}')",
            [&id],
        )
        .unwrap();
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,state,hold_reason,created_at,result_json)VALUES(?1,?1,'null',?2,?1,?3,?4,?5,'now',?6)",params![id,thread,(!waiting).then_some(run),if waiting {"held"} else {"applied"},waiting.then_some(RUN_COMPLETION),if waiting {r#"{"deliver":"complete"}"#} else {r#"{"deliver":"complete","delivery_context":{"reason":"run_completion"}}"#}]).unwrap();
    }
}
