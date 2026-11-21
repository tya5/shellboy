//! Minimal durable scheduler rows. Advancing `next_at` is the at-most-once boundary.
use super::{DbWorker, StorageError};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

pub const MAX_SCHEDULES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueSchedule {
    pub id: Uuid,
    pub thread_id: String,
    pub command: String,
    pub cwd: String,
    pub cron_expression: Option<String>,
    pub scheduled_at: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledCommandResult {
    pub scheduled_at: i64,
    pub finished_at: i64,
    pub status: String,
    pub exit_code: Option<i32>,
    pub output_tail: Option<String>,
    pub output_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledRow {
    pub id: Uuid,
    pub thread_id: String,
    pub command: String,
    pub cwd: String,
    pub cron_expression: Option<String>,
    pub at: Option<i64>,
    pub next_at: Option<i64>,
    pub result: Option<ScheduledCommandResult>,
}

pub async fn next_deadline(db: &DbWorker) -> Result<Option<i64>, StorageError> {
    db.call(|c| {
        c.query_row(
            "SELECT next_at FROM scheduled_command WHERE next_at IS NOT NULL ORDER BY next_at,id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(StorageError::from)
    }).await
}

/// Select the next due row and advance it in one owner-local transaction.
pub async fn advance_due(db: &DbWorker, now: i64) -> Result<Option<DueSchedule>, StorageError> {
    db.call(move |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate = tx
            .query_row(
                "SELECT id,thread_id,command,cwd,cron_expression,next_at FROM scheduled_command WHERE next_at IS NOT NULL AND next_at<=?1 ORDER BY next_at,id LIMIT 1",
                [now],
                |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                )),
            )
            .optional()?;
        let Some((id, thread_id, command, cwd, cron, scheduled_at)) = candidate else {
            tx.commit()?;
            return Ok(None);
        };
        let uuid = Uuid::parse_str(&id)
            .map_err(|error| StorageError::Schema(format!("invalid schedule id: {error}")))?;
        let (next, occurrence_at, calculation_error) = if let Some(expression) = cron.as_deref() {
            let facts = expression.parse::<croner::Cron>().ok().and_then(|parsed| {
                let local = chrono::DateTime::<chrono::Utc>::from_timestamp(now, 0)?
                    .with_timezone(&chrono::Local);
                let latest = parsed.find_previous_occurrence(&local, true).ok()?;
                let future = parsed.find_next_occurrence(&local, false).ok()?;
                Some((future.timestamp(), latest.timestamp().max(scheduled_at)))
            });
            match facts {
                Some((future, latest)) => (Some(future), Some(latest), None),
                None => (
                    None,
                    None,
                    Some("Scheduler: cannot calculate next cron occurrence".to_string()),
                ),
            }
        } else {
            (None, Some(scheduled_at), None)
        };
        let result_at = occurrence_at.unwrap_or(scheduled_at);
        tx.execute(
            "UPDATE scheduled_command SET next_at=?1 WHERE id=?2",
            params![next, id],
        )?;
        if let Some(diagnostic) = calculation_error {
            tx.execute(
                "INSERT INTO scheduled_command_result(schedule_id,scheduled_at,finished_at,status,output_tail) VALUES(?1,?2,?3,'launch_failed',?4) ON CONFLICT(schedule_id) DO UPDATE SET scheduled_at=excluded.scheduled_at,finished_at=excluded.finished_at,status='launch_failed',exit_code=NULL,output_tail=excluded.output_tail,output_truncated=0",
                params![id, result_at, now, diagnostic],
            )?;
            tx.commit()?;
            return Ok(None);
        }
        tx.commit()?;
        Ok(Some(DueSchedule {
            id: uuid,
            thread_id,
            command,
            cwd,
            cron_expression: cron,
            scheduled_at: result_at,
        }))
    })
    .await
}

pub async fn finish(
    db: &DbWorker,
    id: Uuid,
    result: ScheduledCommandResult,
) -> Result<(), StorageError> {
    if !matches!(
        result.status.as_str(),
        "finished" | "overlap_skipped" | "launch_failed" | "timed_out"
    ) || result
        .output_tail
        .as_ref()
        .is_some_and(|s| s.chars().count() > 4096)
    {
        return Err(StorageError::InvalidInput(
            "invalid scheduler result".into(),
        ));
    }
    db.call(move |c| {
        c.execute(
            "INSERT INTO scheduled_command_result(schedule_id,scheduled_at,finished_at,status,exit_code,output_tail,output_truncated) SELECT ?1,?2,?3,?4,?5,?6,?7 WHERE EXISTS(SELECT 1 FROM scheduled_command WHERE id=?1) ON CONFLICT(schedule_id) DO UPDATE SET scheduled_at=excluded.scheduled_at,finished_at=excluded.finished_at,status=excluded.status,exit_code=excluded.exit_code,output_tail=excluded.output_tail,output_truncated=excluded.output_truncated WHERE excluded.scheduled_at>=scheduled_command_result.scheduled_at",
            params![
                id.to_string(),
                result.scheduled_at,
                result.finished_at,
                result.status,
                result.exit_code,
                result.output_tail,
                if result.output_truncated { 1_i64 } else { 0_i64 },
            ],
        )?;
        Ok(())
    })
    .await
}

pub async fn list(db: &DbWorker, thread_id: String) -> Result<Vec<ScheduledRow>, StorageError> {
    db.call(move |c| {
        let mut statement = c.prepare(
            "SELECT s.id,s.thread_id,s.command,s.cwd,s.cron_expression,s.at,s.next_at,r.scheduled_at,r.finished_at,r.status,r.exit_code,r.output_tail,r.output_truncated FROM scheduled_command s JOIN thread t ON t.id=s.thread_id LEFT JOIN scheduled_command_result r ON r.schedule_id=s.id WHERE s.thread_id=?1 AND t.deleted_at IS NULL AND t.archived_at IS NULL ORDER BY s.id",
        )?;
        let rows = statement.query_map([thread_id], |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, String>(1)?,
            r.get::<_, String>(2)?, r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?, r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<i64>>(6)?, r.get::<_, Option<i64>>(7)?,
            r.get::<_, Option<i64>>(8)?, r.get::<_, Option<String>>(9)?,
            r.get::<_, Option<i32>>(10)?, r.get::<_, Option<String>>(11)?,
            r.get::<_, Option<i64>>(12)?,
        )))?;
        let mut output = Vec::new();
        for row in rows {
            let (id, thread_id, command, cwd, cron, at, next_at, scheduled, finished, status, code, tail, truncated) = row?;
            let result = match (scheduled, finished, status) {
                (None, None, None) => None,
                (Some(scheduled_at), Some(finished_at), Some(status)) => {
                    Some(ScheduledCommandResult {
                        scheduled_at,
                        finished_at,
                        status,
                        exit_code: code,
                        output_tail: tail,
                        output_truncated: truncated.unwrap_or(0) != 0,
                    })
                }
                _ => return Err(StorageError::Schema("partial scheduled result".into())),
            };
            output.push(ScheduledRow {
                id: Uuid::parse_str(&id).map_err(|e| StorageError::Schema(e.to_string()))?,
                thread_id, command, cwd, cron_expression: cron, at, next_at, result,
            });
        }
        Ok(output)
    }).await
}
