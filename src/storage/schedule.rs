//! Minimal durable scheduler rows. Advancing `next_at` is the at-most-once boundary.
use super::{DbWorker, StorageError};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueSchedule {
    pub id: Uuid,
    pub command: String,
    pub cwd: String,
    pub cron_expression: Option<String>,
    pub scheduled_at: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledRow {
    pub id: Uuid,
    pub command: String,
    pub cwd: String,
    pub cron_expression: Option<String>,
    pub at: Option<i64>,
    pub next_at: Option<i64>,
    pub result: Option<(i64, i64, String, Option<i32>, Option<String>, bool)>,
}

pub async fn register(
    db: &DbWorker,
    command: String,
    cwd: String,
    cron: Option<String>,
    at: i64,
) -> Result<Uuid, StorageError> {
    if command.trim().is_empty()
        || command.len() > 16_384
        || command.contains('\0')
        || cwd.is_empty()
        || cwd.len() > 4096
        || cwd.contains('\0')
        || at < 0
        || cron.as_ref().is_some_and(|s| s.len() > 256)
    {
        return Err(StorageError::InvalidInput(
            "invalid schedule registration".into(),
        ));
    }
    let (stored_cron, next_at) = if let Some(expression) = cron {
        let parsed = expression
            .parse::<croner::Cron>()
            .map_err(|e| StorageError::InvalidInput(format!("invalid cron expression: {e}")))?;
        let local = chrono::Local::now();
        let next = parsed
            .find_next_occurrence(&local, false)
            .map_err(|e| StorageError::InvalidInput(format!("cron has no next occurrence: {e}")))?;
        (Some(expression), next.timestamp())
    } else {
        (None, at)
    };
    let id = Uuid::new_v4();
    db.call(move|c|{c.execute("INSERT INTO scheduled_command(id,command,cwd,cron_expression,at,next_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id.to_string(),command,cwd,stored_cron,if stored_cron.is_none(){Some(at)}else{None},next_at])?;Ok(())}).await?;
    Ok(id)
}
pub async fn remove(db: &DbWorker, id: Uuid) -> Result<bool, StorageError> {
    db.call(move |c| {
        Ok(c.execute(
            "DELETE FROM scheduled_command WHERE id=?1",
            [id.to_string()],
        )? != 0)
    })
    .await
}
pub async fn next_deadline(db: &DbWorker) -> Result<Option<i64>, StorageError> {
    db.call(|c|c.query_row("SELECT next_at FROM scheduled_command WHERE next_at IS NOT NULL ORDER BY next_at,id LIMIT 1",[],|r|r.get(0)).optional().map_err(StorageError::from)).await
}
/// Query one candidate, calculate recurrence outside SQLite, then CAS-advance in a short transaction before launch.
pub async fn advance_due(db: &DbWorker, now: i64) -> Result<Option<DueSchedule>, StorageError> {
    let candidate=db.call(move|c|{
  c.query_row("SELECT id,command,cwd,cron_expression,next_at FROM scheduled_command WHERE next_at IS NOT NULL AND next_at<=?1 ORDER BY next_at,id LIMIT 1",[now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,i64>(4)?)))
   .optional().map_err(StorageError::from)
 }).await?;
    let Some((id, command, cwd, cron, scheduled_at)) = candidate else {
        return Ok(None);
    };
    let uuid = Uuid::parse_str(&id)
        .map_err(|e| StorageError::Schema(format!("invalid schedule id: {e}")))?;
    let (next, calculation_error) = if let Some(expression) = cron.as_deref() {
        match expression.parse::<croner::Cron>().ok().and_then(|parsed| {
            chrono::DateTime::<chrono::Utc>::from_timestamp(now, 0)
                .map(|utc| utc.with_timezone(&chrono::Local))
                .and_then(|local| parsed.find_next_occurrence(&local, false).ok())
        }) {
            Some(next) => (Some(next.timestamp()), None),
            None => (
                None,
                Some("Scheduler: cannot calculate next cron occurrence".to_string()),
            ),
        }
    } else {
        (None, None)
    };
    db.call(move|c|{let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;let changed=tx.execute("UPDATE scheduled_command SET next_at=?1 WHERE id=?2 AND next_at=?3",params![next,id,scheduled_at])?;if changed==0{tx.rollback()?;return Ok(None)}if let Some(diagnostic)=calculation_error {tx.execute("INSERT INTO scheduled_command_result(schedule_id,scheduled_at,finished_at,status,output_tail) VALUES(?1,?2,?3,'launch_failed',?4) ON CONFLICT(schedule_id) DO UPDATE SET scheduled_at=excluded.scheduled_at,finished_at=excluded.finished_at,status='launch_failed',exit_code=NULL,output_tail=excluded.output_tail,output_truncated=0",params![id,scheduled_at,now,diagnostic])?;tx.commit()?;return Ok(None)}tx.commit()?;Ok(Some(DueSchedule{id:uuid,command,cwd,cron_expression:cron,scheduled_at}))}).await
}
pub async fn finish(
    db: &DbWorker,
    id: Uuid,
    scheduled_at: i64,
    finished_at: i64,
    status: &'static str,
    exit_code: Option<i32>,
    tail: Option<String>,
    truncated: bool,
) -> Result<(), StorageError> {
    if !matches!(
        status,
        "finished" | "overlap_skipped" | "capacity_skipped" | "launch_failed"
    ) || tail.as_ref().is_some_and(|s| s.chars().count() > 4096)
    {
        return Err(StorageError::InvalidInput(
            "invalid scheduler result".into(),
        ));
    }
    db.call(move|c|{c.execute("INSERT INTO scheduled_command_result(schedule_id,scheduled_at,finished_at,status,exit_code,output_tail,output_truncated) SELECT ?1,?2,?3,?4,?5,?6,?7 WHERE EXISTS(SELECT 1 FROM scheduled_command WHERE id=?1) ON CONFLICT(schedule_id) DO UPDATE SET scheduled_at=excluded.scheduled_at,finished_at=excluded.finished_at,status=excluded.status,exit_code=excluded.exit_code,output_tail=excluded.output_tail,output_truncated=excluded.output_truncated WHERE excluded.scheduled_at>=scheduled_command_result.scheduled_at",params![id.to_string(),scheduled_at,finished_at,status,exit_code,tail,if truncated{1_i64}else{0_i64}])?;Ok(())}).await
}
pub async fn list(db: &DbWorker) -> Result<Vec<ScheduledRow>, StorageError> {
    db.call(|c|{let mut st=c.prepare("SELECT s.id,s.command,s.cwd,s.cron_expression,s.at,s.next_at,r.scheduled_at,r.finished_at,r.status,r.exit_code,r.output_tail,r.output_truncated FROM scheduled_command s LEFT JOIN scheduled_command_result r ON r.schedule_id=s.id ORDER BY s.id LIMIT 101")?;let rows=st.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<i64>>(4)?,r.get::<_,Option<i64>>(5)?,r.get::<_,Option<i64>>(6)?,r.get::<_,Option<i64>>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,Option<i32>>(9)?,r.get::<_,Option<String>>(10)?,r.get::<_,Option<i64>>(11)?)))?;let mut out=Vec::new();for row in rows{let(id,command,cwd,cron,at,next,scheduled,finished,status,code,tail,truncated)=row?;out.push(ScheduledRow{id:Uuid::parse_str(&id).map_err(|e|StorageError::Schema(e.to_string()))?,command,cwd,cron_expression:cron,at,next_at:next,result:match(scheduled,finished,status){(None,None,None)=>None,(Some(a),Some(b),Some(c))=>Some((a,b,c,code,tail,truncated.unwrap_or(0)!=0)),_=>return Err(StorageError::Schema("partial scheduled result".into()))}})}Ok(out)}).await
}
