//! Durable reply facts and model-visible notices, inside the caller's transaction.
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(crate) fn admission_notice_tx(
    db: &Connection,
    thread: &str,
    message: &str,
) -> rusqlite::Result<()> {
    let row: Option<(String, Option<String>, String)> = db.query_row(
        "SELECT COALESCE(result_json,'{}'),turn_id,sender_json FROM receipt WHERE target_thread_id=?1 AND message_node_id=?2",
        params![thread, message], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?;
    let Some((saved, turn, sender)) = row else {
        return Ok(());
    };
    let saved: Value = serde_json::from_str(&saved)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let payload = if saved["expect_reply"] == true {
        let sender = crate::protocol::MessageSender::from_saved(Some(&sender));
        Some(
            json!({"kind":"reply_expected","message_id":message,"sender":sender,"command":format!("shellboy message reply --message {message}")}),
        )
    } else if let Some(original) = saved["reply_to"].as_str() {
        let body: Option<String> = db
            .query_row(
                "SELECT payload_json FROM node WHERE id=?1",
                [original],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let excerpt = body
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .map(|p| crate::history::conversation_excerpt(&p, 512))
            .unwrap_or_default();
        Some(
            json!({"kind":"reply","reply_to":original,"excerpt":excerpt.lines().next().unwrap_or("")}),
        )
    } else {
        // A self-asserted label from a non-human sender, so a Thread can tell
        // which front end sent the text. Never identity or authority.
        let sender = crate::protocol::MessageSender::from_saved(Some(&sender));
        sender
            .label
            .filter(|label| !label.is_empty() && sender.kind != crate::protocol::SenderKind::Human)
            .map(|label| {
                json!({"kind":"sender_label","message_id":message,"label":label,"note":"label is self-asserted by the sender and not verified"})
            })
    };
    if let Some(payload) = payload {
        append_notice_tx(
            db,
            thread,
            turn.as_deref(),
            &format!("reply-admission-{message}"),
            payload,
        )?;
    }
    Ok(())
}

fn append_notice_tx(
    db: &Connection,
    thread: &str,
    turn: Option<&str>,
    event: &str,
    payload: Value,
) -> rusqlite::Result<()> {
    let node = format!("event-{event}");
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM node WHERE id=?1)",
        [&node],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    let at = super::state::now();
    let envelope = json!({"event_id":event,"kind":"context_changed","payload":payload});
    let body = json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {envelope}")}],"event_id":event}).to_string();
    let previous: Option<String> = db.query_row(
        "SELECT cursor_node_id FROM thread WHERE id=?1",
        [thread],
        |r| r.get(0),
    )?;
    db.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at) VALUES(?1,'context_changed','{\"kind\":\"shellboy\"}',?2,?3,?4)", params![event,thread,payload.to_string(),at])?;
    db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES(?1,?2,'event',?3,?4,?5,?6)",params![node,turn,at,body,body.len() as i64,crate::digest::sha256_hex(body.as_bytes())])?;
    if let Some(previous) = previous {
        db.execute(
            "INSERT INTO edge(src,dst) VALUES(?1,?2)",
            params![previous, node],
        )?;
    }
    db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) SELECT ?1,COALESCE(MAX(ordinal)+1,0),?2 FROM thread_path WHERE thread_id=?1",params![thread,node])?;
    db.execute("UPDATE thread SET cursor_node_id=?2,selection_revision=selection_revision+1,revision=revision+1,updated_at=?3 WHERE id=?1",params![thread,node,at])?;
    db.execute(
        "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
        [],
    )?;
    if let Some(turn) = turn {
        db.execute("INSERT INTO delivery(event_id,recipient_id,state) SELECT ?1,run_id,'processed' FROM turn WHERE id=?2",params![event,turn])?;
    }
    Ok(())
}

pub(crate) fn awaiting_replies_tx(
    db: &Connection,
    thread: &str,
    human_only: Option<bool>,
) -> rusqlite::Result<Vec<Value>> {
    let mut statement = db.prepare("SELECT r.message_node_id,r.sender_json,r.target_thread_id,r.created_at,r.state,r.result_json,n.payload_json,CAST((julianday('now')-julianday(r.created_at))*86400000 AS INTEGER) FROM receipt r INDEXED BY receipt_reply_pending JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=?1 AND json_extract(r.result_json,'$.reply.state')='pending' AND (?2 IS NULL OR COALESCE(json_extract(r.result_json,'$.to_human'),0)=?2) ORDER BY r.created_at,r.rowid LIMIT 128")?;
    statement
        .query_map(rusqlite::params![thread, human_only.map(i64::from)], |r| {
            let saved: String = r.get(5)?;
            let saved: Value = serde_json::from_str(&saved)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let sender: String = r.get(1)?;
            let payload: Option<String> = r.get(6)?;
            let excerpt = payload
                .and_then(|body| serde_json::from_str::<Value>(&body).ok())
                .map(|value| crate::history::conversation_excerpt(&value, 512))
                .unwrap_or_default();
            let excerpt =
                crate::presentation::sanitize_plain(excerpt.lines().next().unwrap_or(""), 0)
                    .0
                    .chars()
                    .take(512)
                    .collect::<String>();
            Ok(json!({
                "message_id":r.get::<_,String>(0)?,
                "recipient_kind":if saved["to_human"] == true {"human"} else {"thread"},
                "recipient_thread_id":r.get::<_,String>(2)?,
                "sender":crate::protocol::MessageSender::from_saved(Some(&sender)),
                "created_at":r.get::<_,String>(3)?,
                "elapsed_ms":r.get::<_,i64>(7)?.max(0),
                "delivery_status":r.get::<_,String>(4)?,
                "excerpt":excerpt.lines().next().unwrap_or(""),
                "reminded":saved["reply"].get("reminded_at").is_some()
            }))
        })?
        .collect()
}

fn cancelled_notice_tx(
    db: &Connection,
    thread: &str,
    message: &str,
    at: &str,
) -> rusqlite::Result<()> {
    db.execute(
            "INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at) VALUES(?1,'context_changed','{\"kind\":\"shellboy\"}',?2,?3,?4)",
            params![format!("reply-cancelled-{message}"), thread, json!({"kind":"reply_cancelled","message_id":message}).to_string(), at],
        )?;
    Ok(())
}

pub(crate) fn cancel_reply_tx(
    db: &Connection,
    thread: &str,
    message: &str,
    at: &str,
    actor: &Value,
    admitted: bool,
) -> rusqlite::Result<bool> {
    let changed = db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply',json(?2)),updated_at=?3 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,json!({"state":"cancelled","at":at,"cancelled_by":actor,"cancel_reason":"sender"}).to_string(),at])?;
    if changed == 1 {
        let sender: String = db.query_row(
            "SELECT sender_json FROM receipt WHERE message_node_id=?1",
            [message],
            |r| r.get(0),
        )?;
        outcome_tx(db, message, &sender, "cancelled", at)?;
    }
    if changed == 1 && admitted {
        // Leave the current selected path and inflight manifest intact. The
        // ordinary event admission path presents this notice without waking a Run.
        cancelled_notice_tx(db, thread, message, at)?;
    }
    if changed == 1 {
        db.execute(
            "UPDATE thread SET revision=revision+1,updated_at=?2 WHERE id=?1",
            params![thread, at],
        )?;
    }
    Ok(changed == 1)
}

impl super::db::Store {
    pub(super) fn reply(
        &self,
        tx: &rusqlite::Transaction<'_>,
        args: &crate::protocol::MessageReplyArgs,
        caller: Option<&crate::protocol::Origin>,
        operation: &str,
        at: &str,
    ) -> Result<super::db::MutationResult, crate::protocol::ApiError> {
        use crate::protocol::*;
        let sql = |e: rusqlite::Error| crate::app::storage_error(e.into());
        let row = tx.query_row("SELECT target_thread_id,state,turn_id,result_json,sender_json FROM receipt WHERE message_node_id=?1",[&args.message_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,String>(4)?))).optional().map_err(sql)?.ok_or_else(||ApiError::new(ErrorCode::TargetUnavailable,"message not found"))?;
        let saved: Value = row
            .3
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| json!({}));
        if saved["expect_reply"] != true {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "message does not expect a reply",
            ));
        }
        if saved["reply"]["state"] != "pending" {
            return Err(
                ApiError::new(ErrorCode::InvalidState, "reply is already terminal")
                    .details(json!({"reply_state":saved["reply"]["state"]})),
            );
        }
        if row.1 != "applied" {
            return Err(
                ApiError::new(ErrorCode::InvalidState, "message has not been applied")
                    .details(json!({"delivery_status":row.1})),
            );
        }
        if args.text.len() as u64 > self.config.limits.draft_bytes {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "reply exceeds draft byte limit",
            ));
        }
        let by = MessageSender::recorded(caller, args.sender_label.clone(), caller.is_none());
        let sender = MessageSender::from_saved(Some(&row.4));
        let target = sender
            .thread_id
            .as_deref()
            .filter(|_| sender.kind == SenderKind::Model);
        if let Some(sender_thread) = target
            && tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NOT NULL)",
                    [sender_thread],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(sql)?
        {
            let receipt: String = tx
                .query_row(
                    "SELECT id FROM receipt WHERE message_node_id=?1",
                    [&args.message_id],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            deleted_sender_receipt_tx(tx, &receipt, at).map_err(sql)?;
            tx.execute(
                "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
                [],
            )
            .map_err(sql)?;
            return Ok(super::db::MutationResult {
                committed_error: Some(
                    ApiError::new(ErrorCode::InvalidState, "reply sender Thread was deleted")
                        .details(json!({"reason":"sender_deleted","reply_state":"cancelled"})),
                ),
                ..Default::default()
            });
        }
        let target = match target {
            Some(target)
                if tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                        [target],
                        |r| r.get::<_, bool>(0),
                    )
                    .map_err(sql)? =>
            {
                Some(target)
            }
            _ => None,
        };
        let (mut result, node, delivery) = if let Some(target) = target {
            let result = self.send(
                tx,
                &MessageSendArgs {
                    human: caller.is_none(),
                    resume_recovery: saved["to_human"] == true && caller.is_none(),
                    to_human: false,
                    expect_reply: false,
                    sender_label: args.sender_label.clone(),
                    thread_id: Some(target.into()),
                    text: args.text.clone(),
                    deliver: Deliver::Next,
                },
                caller,
                operation,
                at,
            )?;
            let node = result.result["message_id"]
                .as_str()
                .ok_or_else(|| ApiError::new(ErrorCode::InvalidState, "missing reply node"))?
                .to_owned();
            tx.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply_to',?2) WHERE message_node_id=?1",params![node,args.message_id]).map_err(sql)?;
            let delivery = Some(result.result.clone());
            (result, node, delivery)
        } else {
            let node = uuid::Uuid::new_v4().to_string();
            let body = json!({"role":"user","content":[{"type":"input_text","text":args.text}]})
                .to_string();
            tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,?2,'user',?3,?4,?5,?6)",params![node,row.2,at,body,body.len() as i64,crate::digest::sha256_hex(body.as_bytes())]).map_err(sql)?;
            self.index
                .authorize_new_tx(tx, std::slice::from_ref(&node))?;
            (super::db::MutationResult::value(json!({}))?, node, None)
        };
        let reply = json!({"state":"replied","at":at,"node_id":node,"by":by});
        let changed = tx.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply',json(?2)),updated_at=?3 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![args.message_id,reply.to_string(),at]).map_err(sql)?;
        if changed != 1 {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "reply state changed",
            ));
        }
        tx.execute(
            "UPDATE thread SET revision=revision+1,updated_at=?2 WHERE id=?1",
            params![row.0, at],
        )
        .map_err(sql)?;
        tx.execute(
            "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
            [],
        )
        .map_err(sql)?;
        if !result.changed_threads.contains(&row.0) {
            result.changed_threads.push(row.0);
        }
        result.result = json!({"reply_to":args.message_id,"message_id":node});
        if let Some(delivery) = delivery {
            result.result["delivery"] = delivery;
        }
        Ok(result)
    }
}

/// Called only after ordinary completion gates, in the completion transaction.
pub(crate) fn complete_tx(
    db: &Connection,
    thread: &str,
    run: &str,
    at: &str,
    wakes: &mut Vec<String>,
) -> rusqlite::Result<Option<String>> {
    let rows = db.prepare("SELECT message_node_id,sender_json,result_json FROM receipt INDEXED BY receipt_reply_run_pending WHERE run_id=?1 AND state='applied' AND json_extract(result_json,'$.reply.state')='pending' AND json_extract(result_json,'$.to_human') IS NOT 1 ORDER BY rowid LIMIT 128")?
        .query_map([run], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    if !rows.is_empty() {
        db.execute(
            "UPDATE thread SET revision=revision+1 WHERE id=?1",
            [thread],
        )?;
    }
    let mut pending = Vec::new();
    for (message, sender, saved) in rows {
        let value: Value = serde_json::from_str(&saved)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        if value["reply"].get("reminded_at").is_some() {
            if db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','ignored','$.reply.at',?2),updated_at=?2 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,at])? == 1
                && let Some(thread) = outcome_tx(db, &message, &sender, "ignored", at)? { wakes.push(thread); }
        } else {
            db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.reminded_at',?2,'$.reply.reminded_run_id',?3),updated_at=?2 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,at,run])?;
            pending.push(json!({"message_id":message,"sender":crate::protocol::MessageSender::from_saved(Some(&sender))}));
        }
    }
    if pending.is_empty() {
        return Ok(None);
    }
    let event = format!("reply-reminder-{}", uuid::Uuid::new_v4());
    let payload = json!({"kind":"reply_reminder","messages":pending});
    db.execute("INSERT INTO event(id,target_thread_id,target_run_id,kind,payload_ref_json,created_at,source_json) VALUES(?1,?2,?3,'reply_reminder',?4,?5,'{}')",params![event,thread,run,payload.to_string(),at])?;
    db.execute(
        "INSERT INTO delivery(event_id,recipient_id,state) VALUES(?1,?2,'pending')",
        params![event, run],
    )?;
    Ok(Some(event))
}

/// Terminal recipient Runs cannot fulfill their applied requests; settle each once.
pub(crate) fn terminal_tx(db: &Connection, run: &str, at: &str) -> rusqlite::Result<Vec<String>> {
    let mut wakes = Vec::new();
    let rows=db.prepare("SELECT message_node_id,sender_json FROM receipt INDEXED BY receipt_reply_run_pending WHERE run_id=?1 AND state='applied' AND json_extract(result_json,'$.reply.state')='pending' AND json_extract(result_json,'$.to_human') IS NOT 1 AND EXISTS(SELECT 1 FROM run WHERE id=?1 AND state IN ('completed','stopped','failed')) ORDER BY rowid LIMIT 128")?.query_map([run],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (message, sender) in rows {
        if db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','ignored','$.reply.at',?2),updated_at=?2 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,at])?==1
            && let Some(thread)=outcome_tx(db,&message,&sender,"ignored",at)? {wakes.push(thread);}
    }
    Ok(wakes)
}

/// Resume from an indexed pending-receipt position, not a post-filter LIMIT.
/// Cursor loss is safe: pending duties are durable and startup starts a new sweep.
pub(crate) type PendingCursor = (String, i64);
pub(crate) struct OutcomeBatch {
    pub cursor: Option<PendingCursor>,
    pub wakes: Vec<String>,
    pub examined: usize,
}
pub(crate) fn terminal_batch_tx(
    db: &Connection,
    after: Option<PendingCursor>,
    at: &str,
) -> rusqlite::Result<OutcomeBatch> {
    let mut rows = Vec::new();
    if let Some((thread, rowid)) = &after {
        let mut stmt=db.prepare("SELECT target_thread_id,rowid,message_node_id,sender_json,run_id,state,json_extract(result_json,'$.to_human') FROM receipt INDEXED BY receipt_reply_pending WHERE target_thread_id=?1 AND rowid>?2 AND json_extract(result_json,'$.reply.state')='pending' ORDER BY rowid LIMIT 128")?;
        rows.extend(
            stmt.query_map(params![thread, rowid], pending_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    if rows.len() < 128 {
        let mut stmt=db.prepare("SELECT target_thread_id,rowid,message_node_id,sender_json,run_id,state,json_extract(result_json,'$.to_human') FROM receipt INDEXED BY receipt_reply_pending WHERE target_thread_id>?1 AND json_extract(result_json,'$.reply.state')='pending' ORDER BY target_thread_id,rowid LIMIT ?2")?;
        rows.extend(
            stmt.query_map(
                params![
                    after.as_ref().map(|p| p.0.as_str()).unwrap_or(""),
                    (128 - rows.len()) as i64
                ],
                pending_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    let examined = rows.len();
    let cursor = if examined == 128 {
        rows.last().map(|r| (r.0.clone(), r.1))
    } else {
        None
    };
    let mut wakes = Vec::new();
    for (_, _, message, sender, run, state, human) in rows {
        if state != "applied" || human == Some(1) {
            continue;
        }
        let terminal:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND state IN ('completed','stopped','failed'))",[run],|r|r.get(0))?;
        if terminal && db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','ignored','$.reply.at',?2),updated_at=?2 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,at])?==1
            && let Some(thread)=outcome_tx(db,&message,&sender,"ignored",at)? {wakes.push(thread);}
    }
    Ok(OutcomeBatch {
        cursor,
        wakes,
        examined,
    })
}
type PendingRow = (
    String,
    i64,
    String,
    String,
    Option<String>,
    String,
    Option<i64>,
);
fn pending_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

pub(crate) fn cancellation_wakes_tx(
    db: &Connection,
    messages: &[String],
) -> rusqlite::Result<Vec<String>> {
    let mut wakes = Vec::new();
    for message in messages {
        let thread:Option<String>=db.query_row("SELECT e.target_thread_id FROM event e JOIN delivery d ON d.event_id=e.id JOIN run r ON r.id=d.recipient_id WHERE e.id=?1 AND d.state='pending' AND r.state='live'",[format!("reply-outcome-{message}")],|r|r.get(0)).optional()?;
        if let Some(thread) = thread
            && !wakes.contains(&thread)
        {
            wakes.push(thread);
        }
    }
    Ok(wakes)
}

fn outcome_tx(
    db: &Connection,
    message: &str,
    raw_sender: &str,
    state: &str,
    at: &str,
) -> rusqlite::Result<Option<String>> {
    let sender = crate::protocol::MessageSender::from_saved(Some(raw_sender));
    if sender.kind != crate::protocol::SenderKind::Model {
        return Ok(None);
    }
    let Some(thread) = sender.thread_id else {
        return Ok(None);
    };
    let live: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
        [&thread],
        |r| r.get(0),
    )?;
    if !live {
        return Ok(None);
    }
    let run: Option<String> = db
        .query_row(
            "SELECT id FROM run WHERE thread_id=?1 AND state='live'",
            [&thread],
            |r| r.get(0),
        )
        .optional()?;
    let id = format!("reply-outcome-{message}");
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM event WHERE id=?1)",
        [&id],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(None);
    }
    db.execute(
        "UPDATE thread SET revision=revision+1 WHERE id=?1",
        [&thread],
    )?;
    let payload = json!({"kind":"reply_outcome","message_id":message,"state":state});
    let run = match run {
        Some(run) => Some(run),
        None => {
            let stopping: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM run WHERE thread_id=?1 AND state='stopping')",
                [&thread],
                |r| r.get(0),
            )?;
            if stopping {
                None
            } else {
                Some(super::db::start_run_tx(db, &thread, json!({"kind":"shellboy"}), at)?.0)
            }
        }
    };
    db.execute("INSERT INTO event(id,target_thread_id,target_run_id,kind,payload_ref_json,created_at,source_json) VALUES(?1,?2,?3,?4,?5,?6,'{}')",params![id,thread,run,if run.is_some(){"reply_outcome"}else{"context_changed"},payload.to_string(),at])?;
    if let Some(run) = run {
        db.execute(
            "INSERT INTO delivery(event_id,recipient_id,state) VALUES(?1,?2,'pending')",
            params![id, run],
        )?;
        return Ok(Some(thread));
    }
    Ok(None)
}

// Per-recipient pending duties are capped at 128 by send admission.
const DELETED_RECIPIENT_REPLIES: &str = "SELECT message_node_id,sender_json,result_json FROM receipt INDEXED BY receipt_reply_pending WHERE target_thread_id=?1 AND json_extract(result_json,'$.reply.state')='pending' ORDER BY created_at,rowid LIMIT 128";

/// All selected Threads are already marked deleted. No sender-side scan here.
pub(crate) fn deleted_threads_tx(
    db: &Connection,
    targets: &[String],
    at: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut wakes = Vec::new();
    let mut query = db.prepare(DELETED_RECIPIENT_REPLIES)?;
    for target in targets {
        let rows = query
            .query_map([target], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (message, sender, saved) in rows {
            if db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','recipient_deleted','$.reply.at',?2),updated_at=?2 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,at])? == 1 {
                let saved: Value = serde_json::from_str(&saved)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                if saved["to_human"] != true
                    && let Some(thread) = outcome_tx(db, &message, &sender, "recipient_deleted", at)?
                { wakes.push(thread); }
                db.execute("UPDATE thread SET revision=revision+1 WHERE id=?1", [target])?;
            }
        }
    }
    Ok(wakes)
}

/// Called once per row in the finite, at-most-128-row history-GC receipt batch.
pub(crate) fn deleted_sender_receipt_tx(
    db: &Connection,
    receipt: &str,
    at: &str,
) -> rusqlite::Result<()> {
    let row: Option<(String,String,String,String)> = db.query_row(
        "SELECT r.message_node_id,r.target_thread_id,r.sender_json,r.path_admission FROM receipt r WHERE r.id=?1 AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.sender_json,'$.kind')='model' AND EXISTS(SELECT 1 FROM thread s WHERE s.id=json_extract(r.sender_json,'$.thread_id') AND s.deleted_at IS NOT NULL) AND EXISTS(SELECT 1 FROM thread t WHERE t.id=r.target_thread_id AND t.deleted_at IS NULL)",
        [receipt], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).optional()?;
    let Some((message, target, sender, admission)) = row else {
        return Ok(());
    };
    let actor = json!({"kind":"thread_deletion","thread_id":crate::protocol::MessageSender::from_saved(Some(&sender)).thread_id});
    if db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply',json(?2)),updated_at=?3 WHERE id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![receipt,json!({"state":"cancelled","at":at,"cancelled_by":actor,"cancel_reason":"sender_deleted"}).to_string(),at])? == 1 {
        if admission == "admitted" { cancelled_notice_tx(db,&target,&message,at)?; }
        db.execute("UPDATE thread SET revision=revision+1 WHERE id=?1", [target])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_terminal_sweeps_resume_all_senders_without_cross_run_scans_or_duplicates() {
        for state in ["stopped", "completed", "failed"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("synthetic.db");
            let mut db = Connection::open(&path).unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('s1','/','','p','m','0'),('s2','/','','p','m','0'),('target','/','','p','m','0'); INSERT INTO run(id,thread_id,state,created_at) VALUES('r','target','live','0');").unwrap();
            let tx = db.transaction().unwrap();
            for n in 0..4357 {
                let unrelated = n < 4096;
                let message = format!("m-{n:06}");
                let run = if unrelated {
                    format!("other-{n}")
                } else {
                    "r".into()
                };
                if unrelated {
                    tx.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,'target','completed','0')",[&run]).unwrap();
                }
                let sender = json!({"kind":"model","instance_id":"fixture","thread_id":if n%2==0 {"s1"}else{"s2"},"run_id":"sender","turn_id":"sender-turn","execution_id":"sender-shell"});
                tx.execute(
                    "INSERT INTO node(id,kind,payload_json,created_at) VALUES(?1,'user','{}','0')",
                    [&message],
                )
                .unwrap();
                tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,state,path_admission,created_at,result_json) VALUES(?1,?1,?2,'target',?1,?3,?4,'admitted','0','{\"expect_reply\":true,\"reply\":{\"state\":\"pending\"}}')",params![message,sender.to_string(),run,if unrelated{"held"}else{"applied"}]).unwrap();
            }
            tx.execute("UPDATE run SET state=?1 WHERE id='r'", [state])
                .unwrap();
            tx.commit().unwrap();
            let immediate = terminal_tx(&db, "r", "1").unwrap();
            assert_eq!(immediate.len(), 128);
            // A failed transaction cannot advance the in-memory cursor or settle facts.
            let tx = db.transaction().unwrap();
            let rolled = terminal_batch_tx(&tx, None, "2").unwrap();
            assert!(rolled.examined <= 128);
            tx.rollback().unwrap();
            // Reopen discards the cursor exactly as a fresh maintenance owner does.
            drop(db);
            let mut db = Connection::open(&path).unwrap();
            let mut cursor = None;
            let mut finished = false;
            for pass in 0..40 {
                let tx = db.transaction().unwrap();
                let batch = terminal_batch_tx(&tx, cursor, "3").unwrap();
                assert!(batch.examined <= 128);
                cursor = batch.cursor;
                tx.commit().unwrap();
                if pass == 3 {
                    drop(db);
                    db = Connection::open(&path).unwrap();
                    cursor = None;
                }
                if cursor.is_none() && pass != 3 {
                    finished = true;
                    break;
                }
            }
            assert!(finished);
            assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE run_id='r' AND json_extract(result_json,'$.reply.state')='ignored'",[],|r|r.get::<_,i64>(0)).unwrap(),261);
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM event WHERE kind='reply_outcome'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                261
            );
            assert_eq!(db.query_row("SELECT count(*) FROM delivery d JOIN event e ON e.id=d.event_id WHERE e.kind='reply_outcome'",[],|r|r.get::<_,i64>(0)).unwrap(),261);
            assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE state='held' AND json_extract(result_json,'$.reply.state')='pending'",[],|r|r.get::<_,i64>(0)).unwrap(),4096);
            assert!(terminal_tx(&db, "r", "4").unwrap().is_empty());
            let plans=db.prepare("EXPLAIN QUERY PLAN SELECT message_node_id FROM receipt INDEXED BY receipt_reply_run_pending WHERE run_id='r' AND state='applied' AND json_extract(result_json,'$.reply.state')='pending' AND json_extract(result_json,'$.to_human') IS NOT 1 ORDER BY rowid LIMIT 128").unwrap().query_map([],|r|r.get::<_,String>(3)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("SEARCH receipt USING INDEX receipt_reply_run_pending")),
                "{plans:?}"
            );
            assert!(
                !plans.iter().any(|p| p.contains("TEMP B-TREE")),
                "{plans:?}"
            );
        }
    }

    #[test]
    fn terminal_outcomes_wake_completed_sender_once_but_recovery_wait_keeps_pending() {
        for state in [
            "completed",
            "stopped",
            "failed",
            "recovery_wait",
            "cancelled",
        ] {
            let mut db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('s','/','','p','m','0'),('t','/','','p','m','0');
                INSERT INTO run(id,thread_id,state,created_at) VALUES('sender','s','completed','0'),('recipient','t','live','0');
                INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('recipient-turn','t','recipient','running','0');
                INSERT INTO node(id,kind,created_at,payload_json) VALUES('request','user','0','{}');
                INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,path_admission,created_at,result_json) VALUES('request','request','{\"kind\":\"model\",\"thread_id\":\"s\"}','t','request','recipient','recipient-turn','applied','admitted','0','{\"expect_reply\":true,\"reply\":{\"state\":\"pending\"}}');").unwrap();
            let sender = json!({"kind":"model","instance_id":"fixture","thread_id":"s","run_id":"sender","turn_id":"sender-turn","execution_id":"sender-shell"});
            db.execute(
                "UPDATE receipt SET sender_json=?1 WHERE id='request'",
                [sender.to_string()],
            )
            .unwrap();
            let tx = db.transaction().unwrap();
            if state == "recovery_wait" {
                tx.execute(
                    "UPDATE turn SET state='recovery_wait' WHERE id='recipient-turn'",
                    [],
                )
                .unwrap();
            } else if state != "cancelled" {
                tx.execute(
                    "UPDATE run SET state=?1,ended_at='1' WHERE id='recipient'",
                    [state],
                )
                .unwrap();
            }
            let wakes = if state == "cancelled" {
                assert!(
                    cancel_reply_tx(&tx, "t", "request", "1", &json!({"kind":"human"}), true)
                        .unwrap()
                );
                cancellation_wakes_tx(&tx, &["request".into()]).unwrap()
            } else {
                terminal_tx(&tx, "recipient", "1").unwrap()
            };
            let expected = if state == "recovery_wait" {
                "pending"
            } else if state == "cancelled" {
                "cancelled"
            } else {
                "ignored"
            };
            assert_eq!(tx.query_row("SELECT json_extract(result_json,'$.reply.state') FROM receipt WHERE id='request'",[],|r|r.get::<_,String>(0)).unwrap(),expected);
            assert_eq!(
                wakes,
                if state == "recovery_wait" {
                    vec![]
                } else {
                    vec![String::from("s")]
                },
                "{state}"
            );
            if state != "recovery_wait" {
                assert_eq!(tx.query_row("SELECT count(*) FROM delivery d JOIN event e ON e.id=d.event_id JOIN run r ON r.id=d.recipient_id JOIN turn u ON u.run_id=r.id WHERE e.target_thread_id='s' AND d.state='pending' AND r.state='live' AND u.state='preparing'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
                assert!(terminal_tx(&tx, "recipient", "2").unwrap().is_empty());
                assert!(
                    !cancel_reply_tx(&tx, "t", "request", "2", &json!({"kind":"human"}), true)
                        .unwrap()
                );
            }
            tx.commit().unwrap();
        }
    }

    #[test]
    fn deletion_uses_target_index_and_never_touches_unrelated_pending_replies() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        let tx = db.transaction().unwrap();
        for group in 0..33 {
            let target = format!("target-{group}");
            tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','/a','p','m','0')",[&target]).unwrap();
            for n in 0..128 {
                let message = format!("{group}-{n}");
                tx.execute(
                    "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','0','{}')",
                    [&message],
                )
                .unwrap();
                // Some unrelated requests were sent by the deleted Thread too:
                // those must wait for GC, not expand foreground delete work.
                let sender = if group % 2 == 0 {
                    json!({"kind":"model","thread_id":"target-0"})
                } else {
                    json!({"kind":"human"})
                };
                tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,path_admission,created_at,result_json)VALUES(?1,?1,?2,?3,?1,'applied','admitted','0',json_object('reply',json_object('state','pending')))",params![message,sender.to_string(),target]).unwrap();
            }
        }
        tx.commit().unwrap();
        let plan: Vec<String> = db
            .prepare(&format!("EXPLAIN QUERY PLAN {DELETED_RECIPIENT_REPLIES}"))
            .unwrap()
            .query_map(["target-0"], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            plan.iter().any(|s| s
                .contains("SEARCH receipt USING INDEX receipt_reply_pending (target_thread_id=?)")),
            "{plan:?}"
        );
        assert!(!plan.iter().any(|s| s.contains("SCAN receipt")), "{plan:?}");
        db.execute_batch("CREATE TEMP TABLE receipt_updates(id TEXT); CREATE TEMP TRIGGER audit_receipts AFTER UPDATE ON receipt BEGIN INSERT INTO receipt_updates VALUES(NEW.id); END;").unwrap();
        let tx = db.transaction().unwrap();
        tx.execute(
            "UPDATE thread SET deleted_at='deleted' WHERE id='target-0'",
            [],
        )
        .unwrap();
        deleted_threads_tx(&tx, &["target-0".into()], "now").unwrap();
        tx.commit().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM receipt_updates", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            128
        );
        assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE target_thread_id!='target-0' AND json_extract(result_json,'$.reply.state')='pending' AND updated_at=''",[],|r|r.get::<_,i64>(0)).unwrap(),4096);
    }

    #[test]
    fn reminder_rollback_and_restart_keep_once_per_message_not_per_run() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.db");
        let mut db = Connection::open(&path).unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','0'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','0'); INSERT INTO node(id,kind,created_at,payload_json)VALUES('m','user','0','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,state,path_admission,created_at,result_json)VALUES('m','m','{\"kind\":\"human\"}','t','m','r','applied','admitted','0','{\"reply\":{\"state\":\"pending\"}}');").unwrap();
        let tx = db.transaction().unwrap();
        assert!(
            complete_tx(&tx, "t", "r", "one", &mut Vec::new())
                .unwrap()
                .is_some()
        );
        tx.rollback().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM event", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let tx = db.transaction().unwrap();
        assert!(
            complete_tx(&tx, "t", "r", "two", &mut Vec::new())
                .unwrap()
                .is_some()
        );
        tx.commit().unwrap();
        db.execute_batch("UPDATE run SET state='stopped'; INSERT INTO run(id,thread_id,state,created_at)VALUES('new-run','t','live','later');").unwrap();
        drop(db);
        let mut db = Connection::open(path).unwrap();
        let tx = db.transaction().unwrap();
        terminal_batch_tx(&tx, None, "restart").unwrap();
        assert!(
            complete_tx(&tx, "t", "new-run", "three", &mut Vec::new())
                .unwrap()
                .is_none()
        );
        tx.commit().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM event WHERE kind='reply_reminder'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT json_extract(result_json,'$.reply.state') FROM receipt WHERE id='m'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "ignored"
        );
    }
    #[test]
    fn admission_notices_are_once_only_ordered_and_transactional() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','0'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','0'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','0');").unwrap();
        for name in ["a", "b"] {
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','user','0','{\"role\":\"user\",\"content\":[]}')",[name]).unwrap();
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,result_json)VALUES(?1,?1,'{\"kind\":\"external_cli\"}','t',?1,'r','u','accepted','0','{\"expect_reply\":true,\"reply\":{\"state\":\"pending\"}}')",[name]).unwrap();
        }
        let tx = db.transaction().unwrap();
        super::super::message_delivery::append_admitted_tx(
            &tx,
            "t",
            0,
            &["a".into(), "b".into()],
            "now",
        )
        .unwrap();
        let path = tx
            .prepare("SELECT node_id FROM thread_path ORDER BY ordinal")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            path,
            [
                "a",
                "event-reply-admission-a",
                "b",
                "event-reply-admission-b"
            ]
        );
        super::super::message_delivery::mark_admitted_tx(&tx, "t", &["a".into(), "b".into()])
            .unwrap();
        assert_eq!(
            tx.query_row("SELECT count(*) FROM event", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        tx.rollback().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM event", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM receipt WHERE path_admission='admitted'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
