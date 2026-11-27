//! Durable reply facts and outcome notices, inside the caller's transaction.
use super::{
    db::{ReplyIntent, SendContent},
    wait::wakes::Key,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

/// The one reply-duty state rule (#433). An open duty is still owed:
/// `pending`, or `awaiting_own_ask` while the owing Thread waits for an answer
/// to an ask it sent after the duty was admitted. Every other state
/// (`replied`, `ignored`, `cancelled`, `recipient_deleted`) is terminal and never
/// reopens. Blocking, reply, cancel, deletion and Run-end settlement all use this
/// rule. The SQL form is textually identical to the partial-index predicates of
/// migration `0067_open_reply_duties`, so SQLite can prove index use.
macro_rules! open_reply {
    ($alias:literal) => {
        concat!(
            "json_extract(",
            $alias,
            "result_json,'$.reply.state') IN ('pending','awaiting_own_ask')"
        )
    };
}
pub(crate) use open_reply;

/// A duty's owner Run: its delivery Run, or once reopened after an own ask, the
/// Run that admitted that ask's answer or outcome (none until one does).
/// Delivery provenance (`receipt.run_id`) never changes. Matches index
/// `receipt_reply_owner_pending`.
macro_rules! duty_run {
    () => {
        "IIF(json_extract(result_json,'$.reply.reopened_after') IS NULL,run_id,json_extract(result_json,'$.reply.owner_run_id'))"
    };
}

pub(crate) fn is_open(state: Option<&str>) -> bool {
    matches!(state, Some("pending" | "awaiting_own_ask"))
}

pub(crate) fn model_thread_identity(sender: &crate::protocol::MessageSender) -> Option<&str> {
    (sender.kind == crate::protocol::SenderKind::Model)
        .then_some(sender.thread_id.as_deref())
        .flatten()
        .filter(|id| !id.is_empty())
}

/// Open asks a model Thread sent (?1), on their directional partial index. The
/// rewind preview reads the same projection.
macro_rules! asked_by {
    () => {
        "json_extract(r.sender_json,'$.thread_id')=?1 AND json_extract(r.sender_json,'$.kind')='model'"
    };
}
pub(crate) const OPEN_ASKED_FROM: &str = concat!(
    "FROM receipt r INDEXED BY receipt_pending_asked WHERE ",
    open_reply!("r."),
    " AND ",
    asked_by!()
);

fn pending_direction<'a>(
    owed_by: Option<&'a str>,
    asked_by: Option<&'a str>,
    to: Option<&'a str>,
) -> (&'static str, &'static str, &'a str) {
    if let Some(thread) = owed_by {
        (
            "receipt_pending_owed",
            "r.target_thread_id=?1 AND json_extract(r.result_json,'$.ask_name') IS NULL",
            thread,
        )
    } else if let Some(thread) = asked_by {
        ("receipt_pending_asked", asked_by!(), thread)
    } else if let Some(name) = to {
        (
            "receipt_pending_to",
            "json_extract(r.result_json,'$.ask_name')=?1",
            name,
        )
    } else {
        ("receipt_pending_by_time", "?1=''", "")
    }
}

fn count_sql(index: &str, predicate: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM (SELECT 1 FROM receipt r INDEXED BY {index} WHERE {} AND {predicate} LIMIT ?2)",
        open_reply!("r.")
    )
}

fn list_sql(index: &str, predicate: &str) -> String {
    format!(
        "SELECT r.created_at,r.id,r.message_node_id,r.target_thread_id,json_extract(r.result_json,'$.ask_name'),r.sender_json,n.payload_json,json_extract(r.result_json,'$.reply') FROM receipt r INDEXED BY {index} LEFT JOIN node n ON n.id=r.message_node_id WHERE {} AND {predicate} AND (r.created_at,r.id)>(?2,?3) ORDER BY r.created_at,r.id LIMIT ?4",
        open_reply!("r.")
    )
}

/// Open duties this Thread owes that wait on its own ask, on the owed-direction index.
const AWAITING_OWED: &str = concat!(
    "SELECT r.message_node_id,json_extract(r.result_json,'$.reply.blocking_message_id') FROM receipt r INDEXED BY receipt_pending_owed WHERE r.target_thread_id=?1 AND json_extract(r.result_json,'$.ask_name') IS NULL AND ",
    open_reply!("r."),
    " AND json_extract(r.result_json,'$.reply.state')='awaiting_own_ask' ORDER BY r.created_at,r.id LIMIT ?2"
);

pub(super) fn pending_counts(
    db: &Connection,
    thread: &str,
) -> rusqlite::Result<crate::protocol::PendingReplyCounts> {
    let count = |owed_by, asked_by| {
        let (index, predicate, key) = pending_direction(owed_by, asked_by, None);
        let sql = count_sql(index, predicate);
        let cap = crate::protocol::MAX_PENDING_REPLY_DUTIES as i64;
        let count = db.query_row(&sql, params![key, cap + 1], |row| row.get::<_, i64>(0))?;
        Ok::<_, rusqlite::Error>(crate::protocol::PendingReplyCount {
            count: count as u64,
            exact: count <= cap,
        })
    };
    let waiting_on_own_ask = db
        .prepare(AWAITING_OWED)?
        .query_map(
            params![thread, crate::protocol::MAX_PENDING_REPLY_DUTIES as i64],
            |row| {
                Ok(crate::protocol::AwaitingOwnAsk {
                    message_id: row.get(0)?,
                    blocking_message_id: row.get(1)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(crate::protocol::PendingReplyCounts {
        owed_by: count(Some(thread), None)?,
        asked_by: count(None, Some(thread))?,
        awaiting_own_ask: waiting_on_own_ask,
    })
}

pub(super) fn list_pending(
    db: &Connection,
    args: &crate::protocol::MessageListArgs,
) -> Result<crate::protocol::MessageListPage, crate::protocol::ApiError> {
    use crate::protocol::{ApiError, ErrorCode, MessageListFilters, PageCursor, RecordPage};
    let (index, predicate, key) = pending_direction(
        args.owed_by_thread_id.as_deref(),
        args.asked_by_thread_id.as_deref(),
        args.to.as_deref(),
    );
    let scope = "message.list.pending";
    // A combined cursor belongs to both selectors, not the driving index alone.
    let (extra, value) = match (&args.owed_by_thread_id, &args.asked_by_thread_id, &args.to) {
        (Some(_), Some(sender), None) => (
            " AND json_extract(r.sender_json,'$.thread_id')=?5 AND json_extract(r.sender_json,'$.kind')='model'",
            Some(sender),
        ),
        (None, Some(_), Some(recipient)) => (
            " AND json_extract(r.result_json,'$.ask_name')=?5",
            Some(recipient),
        ),
        _ => ("", None),
    };
    let filter = if let Some(value) = value {
        json!([index, key, value])
    } else {
        json!([index, key])
    }
    .to_string();
    let (time, id): (String, String) = if let Some(token) = &args.cursor {
        let cursor = PageCursor::decode_live(token, scope, &filter, 0, None)?;
        serde_json::from_str(&cursor.last_sort_key)
            .map_err(|_| ApiError::new(ErrorCode::CursorExpired, "invalid pending reply cursor"))?
    } else {
        (String::new(), String::new())
    };
    let sql = list_sql(index, &format!("{predicate}{extra}"));
    let read = || -> rusqlite::Result<Vec<_>> {
        let mut stmt = db.prepare(&sql)?;
        let limit = i64::from(args.limit) + 1;
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&key, &time, &id, &limit];
        if let Some(value) = value {
            params.push(value);
        }
        stmt.query_map(params.as_slice(), |row| {
            let time: String = row.get(0)?;
            let id: String = row.get(1)?;
            let thread: String = row.get(3)?;
            let name: Option<String> = row.get(4)?;
            let sender = crate::protocol::MessageSender::from_saved(Some(&row.get::<_,String>(5)?));
            let payload: Option<String> = row.get(6)?;
            let excerpt = payload.and_then(|v|serde_json::from_str::<Value>(&v).ok()).map(|v|crate::history::conversation_excerpt(&v,512)).unwrap_or_default();
            let item = json!({"message_id":row.get::<_,String>(2)?,"created_at":time,"asked_by":model_thread_identity(&sender),"sender":sender,"recipient":match name {Some(name)=>json!({"kind":"name","name":name}),None=>json!({"kind":"thread","thread_id":thread})},"reply":row.get::<_,Option<String>>(7)?.and_then(|v|serde_json::from_str::<Value>(&v).ok()),"excerpt":crate::plain_text::sanitize_plain(excerpt.lines().next().unwrap_or(""),0).0});
            Ok((time,id,item))
        })?.collect()
    };
    let mut rows = read().map_err(|error| crate::app::storage_error(error.into()))?;
    let next_cursor = if rows.len() > args.limit as usize {
        rows.pop();
        let (time, id, _) = rows.last().expect("nonempty page");
        Some(
            PageCursor {
                version: 2,
                scope: scope.into(),
                filter,
                last_sort_key: json!([time, id]).to_string(),
                watermark: 0,
                revision: 0,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    Ok(RecordPage::new(
        args.limit,
        MessageListFilters {
            owed_by_thread_id: args.owed_by_thread_id.clone(),
            asked_by_thread_id: args.asked_by_thread_id.clone(),
            to_omitted: args.to.is_some(),
        },
        rows.into_iter().map(|(_, _, item)| item).collect(),
        next_cursor,
    ))
}

pub(crate) fn pending_duty_ids(
    db: &Connection,
    caller: Option<&crate::protocol::Origin>,
) -> rusqlite::Result<Vec<String>> {
    let Some(origin) = caller else {
        return Ok(Vec::new());
    };
    db.prepare(concat!("SELECT r.message_node_id FROM receipt r INDEXED BY receipt_reply_pending
        JOIN node n ON n.id=r.message_node_id
        WHERE r.target_thread_id=?1 AND ", open_reply!("r."), "
          AND json_extract(r.result_json,'$.ask_name') IS NULL
          AND json_extract(r.result_json,'$.to_human') IS NOT 1
          AND NOT (json_extract(r.sender_json,'$.kind') IS 'model' AND json_extract(r.sender_json,'$.thread_id') IS ?1)
        ORDER BY r.created_at,r.rowid LIMIT ?2"))?
        .query_map(params![origin.thread_id, crate::protocol::MAX_PENDING_REPLY_DUTIES as i64], |row| row.get(0))?
        .collect()
}

fn cancelled_notice_tx(
    db: &Connection,
    thread: &str,
    message: &str,
    at: &str,
) -> rusqlite::Result<()> {
    db.execute(
            "INSERT OR IGNORE INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at) VALUES(?1,'context_changed','{\"kind\":\"shellboy\"}',?2,?3,?4)",
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
    let changed = db.execute(concat!("UPDATE receipt SET result_json=json_set(result_json,'$.reply',json(?2)),updated_at=?3 WHERE message_node_id=?1 AND ", open_reply!("")),params![message,json!({"state":"cancelled","at":at,"cancelled_by":actor,"cancel_reason":"sender"}).to_string(),at])?;
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
        let row = tx
            .query_row(
                "SELECT target_thread_id,state,turn_id,result_json,sender_json FROM receipt WHERE message_node_id=?1",
                [&args.message_id],
                |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                )),
            )
            .optional()
            .map_err(sql)?;
        let unavailable = || {
            pending_duty_ids(tx, caller)
                .map(|ids| {
                    ApiError::new(
                        ErrorCode::TargetUnavailable,
                        "message not found or not owed by this Thread",
                    )
                    .details(json!({"pending_duty_ids": ids}))
                })
                .map_err(sql)
        };
        let Some(row) = row else {
            return Err(unavailable()?);
        };
        let saved: Value = row
            .3
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| json!({}));
        let sender = MessageSender::from_saved(Some(&row.4));
        let target = model_thread_identity(&sender);
        if let Some(origin) = caller {
            if target == Some(origin.thread_id.as_str()) {
                return Err(ApiError::new(
                    ErrorCode::InvalidArgument,
                    "cannot reply to a request you sent",
                )
                .hint("To withdraw your request, use message cancel"));
            }
            if origin.thread_id != row.0
                || saved["to_human"] == true
                || saved["ask_name"].is_string()
            {
                return Err(unavailable()?);
            }
        }
        if saved["expect_reply"] != true {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "message does not expect a reply",
            ));
        }
        if !is_open(saved["reply"]["state"].as_str()) {
            let mut details = json!({"reply_state":saved["reply"]["state"]});
            if saved["reply"]["state"] == "cancelled" && saved["reply"]["cancel_reason"] == "sender"
            {
                details["reason"] = json!("withdrawn");
            }
            return Err(
                ApiError::new(ErrorCode::InvalidState, "reply is already terminal")
                    .details(details),
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
        let by = match saved["ask_name"].as_str() {
            Some("human") => MessageSender::recorded(None, args.sender_label.clone(), true),
            Some(name) => MessageSender::named_reply(name),
            None if saved["to_human"] == true => {
                MessageSender::recorded(None, args.sender_label.clone(), true)
            }
            None if caller.is_none() => {
                MessageSender::recorded(None, args.sender_label.clone(), false)
            }
            None => MessageSender::recorded(caller, None, false),
        };
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
            let mut keys = Vec::new();
            deleted_sender_receipt_tx(tx, &receipt, at, &mut keys).map_err(sql)?;
            tx.execute(
                "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
                [],
            )
            .map_err(sql)?;
            return Ok(super::db::MutationResult {
                wait_keys: keys,
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
            let result = self.send_content(
                tx,
                &MessageSendArgs {
                    max_model_requests: None,
                    // Preserve reply assignment independently of its recorded sender.
                    human: caller.is_none(),
                    resume_recovery: saved["to_human"] == true && caller.is_none(),
                    sender_label: None,
                    thread_id: Some(target.into()),
                    text: args.text.clone(),
                    deliver: Deliver::Next,
                },
                caller,
                operation,
                at,
                SendContent {
                    reply: ReplyIntent::None,
                    elements: vec![json!({"type":"input_text","text":args.text})],
                    sender: by.clone(),
                },
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
        let changed = tx.execute(concat!("UPDATE receipt SET result_json=json_set(result_json,'$.reply',json(?2)),updated_at=?3 WHERE message_node_id=?1 AND ", open_reply!("")),params![args.message_id,reply.to_string(),at]).map_err(sql)?;
        if changed != 1 {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "reply state changed",
            ));
        }
        if let Some(asker) = target {
            // The answer was just delivered to the asker's live Run (or a new one).
            reopen_awaiting_tx(tx, &args.message_id, asker, at).map_err(sql)?;
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
        result
            .wait_keys
            .push(super::wait::wakes::Key::Reply(args.message_id.clone()));
        result.result = json!({"reply_to":args.message_id,"message_id":node});
        if let Some(delivery) = delivery {
            result.result["delivery"] = delivery;
        }
        Ok(result)
    }
}

/// Pending applied duties owned by one Run (see `duty_run!`).
const RUN_DUTIES: &str = concat!(
    "SELECT message_node_id,sender_json,result_json FROM receipt INDEXED BY receipt_reply_owner_pending WHERE ",
    duty_run!(),
    "=?1 AND state='applied' AND json_extract(result_json,'$.reply.state')='pending' AND json_extract(result_json,'$.to_human') IS NOT 1 ORDER BY rowid LIMIT 128"
);

/// The Thread's first open ask whose saved send time follows the duty's saved
/// admission time, on the asked-direction index.
const LATER_OWN_ASK: &str = concat!(
    "SELECT q.message_node_id FROM receipt q INDEXED BY receipt_pending_asked WHERE json_extract(q.sender_json,'$.thread_id')=?1 AND json_extract(q.sender_json,'$.kind')='model' AND ",
    open_reply!("q."),
    " AND q.created_at>?2 ORDER BY q.created_at,q.id LIMIT 1"
);

/// Called only after ordinary completion gates, in the completion transaction.
pub(crate) fn complete_tx(
    db: &Connection,
    thread: &str,
    run: &str,
    at: &str,
    wakes: &mut Vec<String>,
    keys: &mut Vec<Key>,
) -> rusqlite::Result<Option<String>> {
    let rows = db
        .prepare(RUN_DUTIES)?
        .query_map([run], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

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
        let reply = &value["reply"];
        // Only saved admission evidence orders an ask after the duty; a duty
        // admitted before this evidence was saved is never blocked.
        let blocking: Option<String> = match reply["admitted_at"].as_str() {
            Some(admitted) => db
                .query_row(LATER_OWN_ASK, params![thread, admitted], |row| row.get(0))
                .optional()?,
            None => None,
        };
        if let Some(blocking) = blocking {
            db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','awaiting_own_ask','$.reply.blocking_message_id',?2),updated_at=?3 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,blocking,at])?;
            continue;
        }
        // A reopened duty already had its one wake: the answer or outcome of
        // the ask it awaited.
        if reply.get("reminded_at").is_some() || reply.get("reopened_after").is_some() {
            if db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','ignored','$.reply.at',?2),updated_at=?2 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,at])? == 1
                { keys.push(Key::Reply(message.clone())); if let Some(thread) = outcome_tx(db, &message, &sender, "ignored", at)? { wakes.push(thread); } }
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

/// Terminal recipient Runs cannot fulfill the pending duties they own; settle
/// each once. A duty awaiting its own ask is not owned by any Run until reopened.
pub(crate) fn terminal_tx(
    db: &Connection,
    run: &str,
    at: &str,
    keys: &mut Vec<Key>,
) -> rusqlite::Result<Vec<String>> {
    let mut wakes = Vec::new();
    let terminal: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND state IN ('completed','stopped','failed'))",
        [run],
        |r| r.get(0),
    )?;
    if !terminal {
        return Ok(wakes);
    }
    let rows = db
        .prepare(RUN_DUTIES)?
        .query_map([run], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (message, sender) in rows {
        if db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','ignored','$.reply.at',?2),updated_at=?2 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,at])?==1
            { keys.push(Key::Reply(message.clone())); if let Some(thread)=outcome_tx(db,&message,&sender,"ignored",at)? {wakes.push(thread);} }
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
    pub keys: Vec<Key>,
}
const SWEEP_COLUMNS: &str = concat!(
    "SELECT target_thread_id,rowid,message_node_id,sender_json,",
    duty_run!(),
    ",state,json_extract(result_json,'$.to_human'),json_extract(result_json,'$.reply.state') FROM receipt INDEXED BY receipt_reply_pending WHERE "
);
const SWEEP_IN_THREAD: &str = concat!(
    "target_thread_id=?1 AND rowid>?2 AND ",
    open_reply!(""),
    " ORDER BY rowid LIMIT 128"
);
const SWEEP_LATER_THREADS: &str = concat!(
    "target_thread_id>?1 AND ",
    open_reply!(""),
    " ORDER BY target_thread_id,rowid LIMIT ?2"
);
pub(crate) fn terminal_batch_tx(
    db: &Connection,
    after: Option<PendingCursor>,
    at: &str,
) -> rusqlite::Result<OutcomeBatch> {
    let mut rows = Vec::new();
    if let Some((thread, rowid)) = &after {
        let mut stmt = db.prepare(&format!("{SWEEP_COLUMNS}{SWEEP_IN_THREAD}"))?;
        rows.extend(
            stmt.query_map(params![thread, rowid], pending_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    if rows.len() < 128 {
        let mut stmt = db.prepare(&format!("{SWEEP_COLUMNS}{SWEEP_LATER_THREADS}"))?;
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
    let mut keys = Vec::new();
    for (_, _, message, sender, run, state, human, reply) in rows {
        // Only an owned pending duty can be settled; one awaiting its own ask
        // stays open whatever happened to the Run that delivered it.
        if state != "applied" || human == Some(1) || reply != "pending" {
            continue;
        }
        let terminal:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND state IN ('completed','stopped','failed'))",[run],|r|r.get(0))?;
        if terminal && db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','ignored','$.reply.at',?2),updated_at=?2 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,at])?==1
            { keys.push(Key::Reply(message.clone())); if let Some(thread)=outcome_tx(db,&message,&sender,"ignored",at)? {wakes.push(thread);} }
    }
    Ok(OutcomeBatch {
        cursor,
        wakes,
        examined,
        keys,
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
    String,
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
        row.get(7)?,
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

/// The asker Thread's ask just became terminal and this transaction saved its
/// answer or outcome for the asker. Every duty the Thread awaited on that ask is
/// owed again (`reply.reopened_after` names the ask) but has no owner Run until
/// a Run admits that answer or outcome (`claim_reopened_tx`), so no Run's end or
/// stop settles it before then. The duty keeps its delivery Run and Turn.
const REOPEN_AWAITING: &str = concat!(
    "UPDATE receipt INDEXED BY receipt_pending_owed SET result_json=json_set(json_remove(result_json,'$.reply.blocking_message_id','$.reply.owner_run_id'),'$.reply.state','pending','$.reply.reopened_after',?1),updated_at=?3 WHERE target_thread_id=?2 AND json_extract(result_json,'$.ask_name') IS NULL AND ",
    open_reply!(""),
    " AND json_extract(result_json,'$.reply.state')='awaiting_own_ask' AND json_extract(result_json,'$.reply.blocking_message_id')=?1"
);
fn reopen_awaiting_tx(db: &Connection, ask: &str, asker: &str, at: &str) -> rusqlite::Result<()> {
    db.execute(REOPEN_AWAITING, params![ask, asker, at])?;
    Ok(())
}

/// The Run that admits a reopened duty's answer (the reply node on the
/// Thread's selected path) or its outcome notice (a delivery to this Run)
/// becomes the duty's owner Run.
const CLAIM_REOPENED: &str = concat!(
    "UPDATE receipt INDEXED BY receipt_pending_owed SET result_json=json_set(result_json,'$.reply.owner_run_id',?2),updated_at=?3 WHERE target_thread_id=?1 AND json_extract(result_json,'$.ask_name') IS NULL AND ",
    open_reply!(""),
    " AND json_extract(result_json,'$.reply.state')='pending' AND json_extract(result_json,'$.reply.reopened_after') IS NOT NULL AND json_extract(result_json,'$.reply.owner_run_id') IS NULL AND (EXISTS(SELECT 1 FROM receipt q JOIN thread_path p ON p.node_id=json_extract(q.result_json,'$.reply.node_id') AND p.thread_id=?1 WHERE q.message_node_id=json_extract(receipt.result_json,'$.reply.reopened_after')) OR EXISTS(SELECT 1 FROM delivery d WHERE d.event_id='reply-outcome-'||json_extract(receipt.result_json,'$.reply.reopened_after') AND d.recipient_id=?2))"
);
/// Outcome notices no Run has admitted yet, offered to a Turn that is already
/// runnable (never a wake), like untargeted context observations.
const OFFER_UNADMITTED_OUTCOMES: &str = "INSERT OR IGNORE INTO delivery(event_id,recipient_id,state) SELECT e.id,?2,'pending' FROM event e INDEXED BY reply_outcome_unadmitted WHERE e.target_thread_id=?1 AND e.kind='reply_outcome' AND e.target_run_id IS NULL AND NOT EXISTS(SELECT 1 FROM delivery d WHERE d.event_id=e.id AND d.state='processed')";
pub(crate) fn offer_unadmitted_outcomes_tx(
    db: &Connection,
    thread: &str,
    run: &str,
) -> rusqlite::Result<()> {
    db.execute(OFFER_UNADMITTED_OUTCOMES, params![thread, run])?;
    Ok(())
}

/// Called by Turn preparation after it admitted this Run's input and notices.
pub(crate) fn claim_reopened_tx(
    db: &Connection,
    thread: &str,
    run: &str,
    at: &str,
) -> rusqlite::Result<()> {
    db.execute(CLAIM_REOPENED, params![thread, run, at])?;
    Ok(())
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
                Some(super::db::start_run_tx(db, &thread, json!({"kind":"shellboy"}), at, None)?.0)
            }
        }
    };
    // The notice belongs to the Thread until a Run admits it (#433): it stays
    // untargeted, and the delivery below only wakes the receiving Run. If that
    // Run stops first, the Thread's next runnable Turn admits it.
    db.execute("INSERT INTO event(id,target_thread_id,target_run_id,kind,payload_ref_json,created_at,source_json) VALUES(?1,?2,NULL,'reply_outcome',?3,?4,'{}')",params![id,thread,payload.to_string(),at])?;
    reopen_awaiting_tx(db, message, &thread, at)?;
    if let Some(run) = run {
        db.execute(
            "INSERT INTO delivery(event_id,recipient_id,state) VALUES(?1,?2,'pending')",
            params![id, run],
        )?;
        return Ok(Some(thread));
    }
    Ok(None)
}

// Thread deletion handles at most this many receipts synchronously. The
// durable history-GC receipt cursor handles any remaining pending replies.
const DELETED_RECIPIENT_REPLIES: &str = concat!(
    "SELECT message_node_id,sender_json,result_json FROM receipt INDEXED BY receipt_reply_pending WHERE target_thread_id=?1 AND ",
    open_reply!(""),
    " ORDER BY created_at,rowid LIMIT ?2"
);

pub(crate) fn deleted_threads_tx(
    db: &Connection,
    targets: &[String],
    at: &str,
    keys: &mut Vec<Key>,
) -> rusqlite::Result<Vec<String>> {
    let mut wakes = std::collections::BTreeSet::new();
    let mut budget = crate::protocol::MAX_SYNC_DELETION_REPLY_DUTIES as usize;
    let mut query = db.prepare(DELETED_RECIPIENT_REPLIES)?;
    for target in targets {
        if budget == 0 {
            break;
        }
        let rows = query
            .query_map(params![target, budget as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        budget = budget.saturating_sub(rows.len());
        for (message, sender, saved) in rows {
            if db.execute(concat!("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','recipient_deleted','$.reply.at',?2),updated_at=?2 WHERE message_node_id=?1 AND ", open_reply!("")),params![message,at])? == 1 {
                keys.push(Key::Reply(message.clone()));
                let saved: Value = serde_json::from_str(&saved)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                if saved["to_human"] != true
                    && let Some(thread) = outcome_tx(db, &message, &sender, "recipient_deleted", at)?
                { wakes.insert(thread); }
                db.execute("UPDATE thread SET revision=revision+1 WHERE id=?1", [target])?;
            }
        }
    }
    Ok(wakes.into_iter().collect())
}

/// Durable history-GC continuation for a reply whose recipient Thread was
/// deleted. Called for one receipt in a fixed-size GC transaction; the caller's
/// rowid cursor makes a crash/restart resume after this outcome commit.
pub(crate) fn deleted_recipient_receipt_tx(
    db: &Connection,
    receipt: &str,
    at: &str,
    keys: &mut Vec<Key>,
) -> rusqlite::Result<Option<String>> {
    let row: Option<(String, String, String, String, String)> = db.query_row(
        concat!("SELECT r.message_node_id,r.sender_json,r.target_thread_id,COALESCE(r.result_json,'{}'),r.path_admission FROM receipt r JOIN thread t ON t.id=r.target_thread_id WHERE r.id=?1 AND ", open_reply!("r."), " AND t.deleted_at IS NOT NULL"),
        [receipt],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ).optional()?;
    let Some((message, sender, target, saved, admission)) = row else {
        return Ok(None);
    };
    let mut wake = None;
    if db.execute(concat!("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','recipient_deleted','$.reply.at',?2),updated_at=?2 WHERE id=?1 AND ", open_reply!("")), params![receipt, at])? == 1 {
        keys.push(Key::Reply(message.clone()));
        let saved: Value = serde_json::from_str(&saved)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        if saved["to_human"] != true {
            // outcome_tx stores the sender event and delivery atomically with
            // this receipt transition; dispatch occurs only after this commits.
            wake = outcome_tx(db, &message, &sender, "recipient_deleted", at)?;
        } else if admission == "admitted" {
            cancelled_notice_tx(db, &target, &message, at)?;
        }
        db.execute("UPDATE thread SET revision=revision+1 WHERE id=?1", [target])?;
    }
    Ok(wake)
}

/// Called once per row in the finite, at-most-128-row history-GC receipt batch.
pub(crate) fn deleted_sender_receipt_tx(
    db: &Connection,
    receipt: &str,
    at: &str,
    keys: &mut Vec<Key>,
) -> rusqlite::Result<()> {
    let row: Option<(String,String,String,String)> = db.query_row(
        concat!("SELECT r.message_node_id,r.target_thread_id,r.sender_json,r.path_admission FROM receipt r WHERE r.id=?1 AND ", open_reply!("r."), " AND json_extract(r.sender_json,'$.kind')='model' AND EXISTS(SELECT 1 FROM thread s WHERE s.id=json_extract(r.sender_json,'$.thread_id') AND s.deleted_at IS NOT NULL) AND EXISTS(SELECT 1 FROM thread t WHERE t.id=r.target_thread_id AND t.deleted_at IS NULL)"),
        [receipt], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).optional()?;
    let Some((message, target, sender, admission)) = row else {
        return Ok(());
    };
    let actor = json!({"kind":"thread_deletion","thread_id":crate::protocol::MessageSender::from_saved(Some(&sender)).thread_id});
    if db.execute(concat!("UPDATE receipt SET result_json=json_set(result_json,'$.reply',json(?2)),updated_at=?3 WHERE id=?1 AND ", open_reply!("")),params![receipt,json!({"state":"cancelled","at":at,"cancelled_by":actor,"cancel_reason":"sender_deleted"}).to_string(),at])? == 1 {
        keys.push(Key::Reply(message.clone()));
        if admission == "admitted" { cancelled_notice_tx(db,&target,&message,at)?; }
        db.execute("UPDATE thread SET revision=revision+1 WHERE id=?1", [target])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_duty_reads_seek_state_qualified_indexes_for_both_open_states() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        let plan = |sql: &str| {
            let mut stmt = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            // Parameters stay unbound: the plan does not depend on their values.
            let mut rows = stmt.raw_query();
            let mut steps = Vec::new();
            while let Some(row) = rows.next().unwrap() {
                steps.push(row.get::<_, String>(3).unwrap());
            }
            steps
        };
        let mut reads = Vec::new();
        for (owed, asked, to) in [
            (Some("t"), None, None),
            (None, Some("t"), None),
            (None, None, Some("human")),
            (None, None, None),
        ] {
            let (index, predicate, _) = pending_direction(owed, asked, to);
            reads.push((index, list_sql(index, predicate)));
            if to.is_none() && (owed.is_some() || asked.is_some()) {
                reads.push((index, count_sql(index, predicate)));
            }
        }
        reads.push(("receipt_pending_owed", AWAITING_OWED.into()));
        reads.push(("receipt_pending_asked", LATER_OWN_ASK.into()));
        reads.push(("receipt_pending_owed", REOPEN_AWAITING.into()));
        reads.push(("receipt_pending_owed", CLAIM_REOPENED.into()));
        reads.push(("reply_outcome_unadmitted", OFFER_UNADMITTED_OUTCOMES.into()));
        for sql in crate::history::rewind::pending_replies_sent_sql() {
            reads.push(("receipt_pending_asked", sql));
        }
        reads.push(("receipt_reply_owner_pending", RUN_DUTIES.into()));
        for (index, sql) in reads {
            let steps = plan(&sql);
            assert!(
                steps.iter().any(|s| s.starts_with("SEARCH")
                    && (s.contains(&format!("USING INDEX {index} "))
                        || s.contains(&format!("USING COVERING INDEX {index} ")))),
                "{index}: {steps:?}"
            );
            assert!(
                !steps.iter().any(|s| s.contains("TEMP B-TREE")),
                "{steps:?}"
            );
        }
    }

    #[test]
    fn admitted_messages_replay_receipt_facts_without_notice_nodes() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','0'); INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','live','0'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','running','0');").unwrap();
        let senders = [
            json!({"kind":"model","instance_id":"fixture-instance","thread_id":"fixture-origin","run_id":"fixture-run","turn_id":"fixture-turn","execution_id":"fixture-execution","label":"fixture-request"}),
            json!({"kind":"external_cli","label":"fixture-other"}),
            json!({"kind":"external_cli","label":"fixture-answer"}),
        ];
        for ((id, facts), sender) in [
            ("request", json!({"expect_reply":true})),
            ("other", json!({})),
            ("answer", json!({"reply_to":"request"})),
        ]
        .into_iter()
        .zip(&senders)
        {
            let body =
                json!({"role":"user","content":[{"type":"input_text","text":id}]}).to_string();
            let sender = sender.to_string();
            db.execute("INSERT INTO node(id,kind,turn_id,created_at,payload_json,payload_bytes) VALUES(?1,'user','u','0',?2,length(?2))",params![id,body]).unwrap();
            let mut facts = facts;
            facts["message_id"] = json!(id);
            facts["thread_id"] = json!("t");
            facts["delivery_status"] = json!("accepted");
            facts["assignment"] = json!("pending");
            if id == "request" {
                facts["reply"] = json!({"state":"pending"});
            }
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES(?1,?1,?2,'t',?1,'accepted','0',?3)",params![id,sender,facts.to_string()]).unwrap();
        }
        let tx = db.transaction().unwrap();
        let ids = ["request".into(), "other".into(), "answer".into()];
        super::super::message_delivery::append_admitted_tx(&tx, "t", 0, &ids, "now").unwrap();
        let count: u32 = tx
            .query_row("SELECT count(*) FROM thread_path", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 3,
            "admission must append only the three received bodies"
        );
        for (id, kind) in [
            ("event-reply-admission-request", "event"),
            ("event-reply-admission-other", "message"),
        ] {
            let body = json!({"role":"developer","content":[{"type":"input_text","text":"old duplicate"}]});
            crate::history::append_node(&tx, "t", id, Some("u"), kind, &body, "now").unwrap();
        }
        let notification = json!({"kind":"shell_ended","execution_id":"fixture-shell","state":"exited","exit_code":0});
        tx.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at,semantic_json) VALUES('shell-finished','context_changed','{}','t','{}','now',?1)", [notification.to_string()]).unwrap();
        let notice = json!({"event_id":"shell-finished","role":"developer","content":[{"type":"input_text","text":"fixture shell finished"}]});
        crate::history::append_node(
            &tx,
            "t",
            "event-shell-finished",
            Some("u"),
            "event",
            &notice,
            "now",
        )
        .unwrap();
        tx.execute("UPDATE node SET notification_event_id='shell-finished' WHERE id='event-shell-finished'", []).unwrap();
        let cache = super::super::replay::ReplayCache::new(1024 * 1024);
        let wire = cache.load_and_commit(tx, "t").unwrap().materialize();
        assert_eq!(wire, cache.load(&db, "t").unwrap().materialize());
        assert_eq!(
            wire,
            super::super::replay::load_uncached(&db, "t", None, 0)
                .unwrap()
                .materialize()
        );
        let text: Vec<_> = wire
            .iter()
            .map(|item| item["content"][0]["text"].as_str().unwrap())
            .collect();
        assert_eq!(text.len(), 7);
        assert!(text[0].contains("shellboy message reply --message request"));
        assert_eq!(text[1], "request");
        assert!(text[2].contains("fixture-other"));
        assert_eq!(text[3], "other");
        assert!(text[4].contains("request"));
        assert_eq!(text[5], "answer");
        assert_eq!(text[6], "fixture shell finished");
        let args = |items| crate::protocol::HistoryWindowArgs {
            include_observation: false,
            observe_node_ids: Vec::new(),
            thread_id: "t".into(),
            items,
            ordinal: Some(0),
            node_id: None,
            expect_node_id: None,
            before: 0,
            after: 5,
            excerpt_chars: 32,
        };
        assert_eq!(
            crate::history::window(&db, &args(false))
                .unwrap()
                .items
                .len(),
            6
        );
        let out = serde_json::to_value(crate::history::window(&db, &args(true)).unwrap()).unwrap();
        assert_eq!(
            out["items"][0]["provenance"]["message"]["expect_reply"],
            true
        );
        assert_eq!(
            out["items"][2]["provenance"]["message"]["reply_to"],
            "request"
        );
        for (i, (id, sender)) in ["request", "other", "answer"]
            .into_iter()
            .zip(senders)
            .enumerate()
        {
            let row = &out["items"][i];
            assert_eq!(row["node_id"], id);
            assert_eq!(row["provenance"]["sender"], sender, "sender for {id}");
        }
        let real = &out["items"][5];
        assert_eq!(real["node_id"], "event-shell-finished");
        assert_eq!(real["kind"], "event");
        assert_eq!(real["provenance"]["notification"], notification);
        assert_eq!(real["items"][0]["text"], "fixture shell finished");
        for i in [3, 4] {
            assert_eq!(out["items"][i]["items"], json!([]));
        }
    }

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
            for (query, args) in [
                (RUN_DUTIES.to_owned(), params!["r"]),
                (
                    format!("{SWEEP_COLUMNS}{SWEEP_LATER_THREADS}"),
                    params!["a", 128],
                ),
                (
                    format!("{SWEEP_COLUMNS}{SWEEP_IN_THREAD}"),
                    params!["target", 1],
                ),
            ] {
                let plans = db
                    .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                    .unwrap()
                    .query_map(args, |r| r.get::<_, String>(3))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                assert!(
                    plans
                        .iter()
                        .any(|p| p.contains("SEARCH receipt USING INDEX")),
                    "{plans:?}"
                );
                assert!(
                    !plans.iter().any(|p| p.contains("TEMP B-TREE")),
                    "{plans:?}"
                );
                let mut stmt = db.prepare(&query).unwrap();
                let count = stmt
                    .query_map(args, |_| Ok(()))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap()
                    .len();
                assert_eq!(count, 128);
            }
            let immediate = terminal_tx(&db, "r", "1", &mut Vec::new()).unwrap();
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
            assert_eq!(
                named_reply_states(
                    &db,
                    message_ids(&db, "SELECT message_node_id FROM receipt WHERE run_id='r'")
                )
                .get("ignored")
                .copied(),
                Some(261)
            );
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
            assert_eq!(
                named_reply_states(
                    &db,
                    message_ids(
                        &db,
                        "SELECT message_node_id FROM receipt WHERE state='held'"
                    )
                )
                .get("pending")
                .copied(),
                Some(4096)
            );
            assert!(
                terminal_tx(&db, "r", "4", &mut Vec::new())
                    .unwrap()
                    .is_empty()
            );
            let plans = db
                .prepare(&format!("EXPLAIN QUERY PLAN {RUN_DUTIES}"))
                .unwrap()
                .query_map(["r"], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("SEARCH receipt USING INDEX receipt_reply_owner_pending")),
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
                terminal_tx(&tx, "recipient", "1", &mut Vec::new()).unwrap()
            };
            let expected = if state == "recovery_wait" {
                "pending"
            } else if state == "cancelled" {
                "cancelled"
            } else {
                "ignored"
            };
            assert_eq!(named_reply_state(&tx, "request"), expected);
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
                assert!(
                    terminal_tx(&tx, "recipient", "2", &mut Vec::new())
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    !cancel_reply_tx(&tx, "t", "request", "2", &json!({"kind":"human"}), true)
                        .unwrap()
                );
            }
            tx.commit().unwrap();
        }
    }

    fn named_reply_state(db: &Connection, message: &str) -> String {
        named_reply_states(db, [message.to_owned()])
            .into_keys()
            .next()
            .unwrap()
    }

    fn message_ids(db: &Connection, sql: &str) -> Vec<String> {
        db.prepare(sql)
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Reply states as the owner's named-message read reports them.
    fn named_reply_states(
        db: &Connection,
        messages: impl IntoIterator<Item = String>,
    ) -> std::collections::BTreeMap<String, usize> {
        let mut states = std::collections::BTreeMap::new();
        for message in messages {
            let details = crate::control::db::message_details(db, &message).unwrap();
            let state = details
                .reply
                .as_ref()
                .and_then(|reply| reply["state"].as_str())
                .unwrap_or("none")
                .to_owned();
            *states.entry(state).or_default() += 1;
        }
        states
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
            .query_map(
                params!["target-0", crate::protocol::MAX_PENDING_REPLY_DUTIES as i64],
                |r| r.get(3),
            )
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
        deleted_threads_tx(&tx, &["target-0".into()], "now", &mut Vec::new()).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM receipt_updates", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            16
        );
        assert_eq!(
            named_reply_states(&db, (0..128).map(|n| format!("0-{n}"))),
            std::collections::BTreeMap::from([
                ("pending".to_owned(), 112),
                ("recipient_deleted".to_owned(), 16),
            ])
        );
        assert_eq!(
            named_reply_states(
                &db,
                (1..33).flat_map(|group| (0..128).map(move |n| format!("{group}-{n}")))
            ),
            std::collections::BTreeMap::from([("pending".to_owned(), 4096)])
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM receipt WHERE target_thread_id!='target-0' AND updated_at=''",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            4096,
            "unrelated receipts are untouched"
        );
    }

    #[test]
    fn deleted_reply_sync_work_is_capped_for_17_and_129_legacy_rows() {
        for count in [17, 129] {
            let mut db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('target','','','p','m','now')", []).unwrap();
            for index in 0..count {
                let message = format!("message-{index}");
                db.execute(
                    "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}')",
                    [&message],
                )
                .unwrap();
                db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)VALUES(?1,?1,'{\"kind\":\"human\"}','target',?1,'applied','now','{\"reply\":{\"state\":\"pending\"},\"to_human\":false}')", [&message]).unwrap();
            }
            let tx = db.transaction().unwrap();
            let wakes =
                deleted_threads_tx(&tx, &["target".into()], "now", &mut Vec::new()).unwrap();
            assert!(wakes.is_empty());
            tx.commit().unwrap();
            assert_eq!(
                named_reply_states(&db, (0..count).map(|index| format!("message-{index}"))),
                std::collections::BTreeMap::from([
                    ("pending".to_owned(), count - 16),
                    ("recipient_deleted".to_owned(), 16),
                ])
            );
        }
    }

    #[test]
    fn all_existing_question_reply_terminal_writers_preserve_canonical_terminal_facts() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at,deleted_at) VALUES('bulk-target','','','p','m','now','now'),('gc-target','','','p','m','now','now'),('live-target','','','p','m','now',NULL),('deleted-sender','','','p','m','now','now');").unwrap();
        let add = |message: &str, target: &str, sender: &str| {
            db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'assistant','now','{}')",[message]).unwrap();
            db.execute(r#"INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,path_admission,created_at,result_json)VALUES(?1,?1,?2,?3,?1,'applied','unadmitted','now','{"to_human":true,"reply":{"state":"pending"}}')"#,rusqlite::params![message,sender,target]).unwrap();
        };
        add(
            "bulk-question",
            "bulk-target",
            &json!({"kind":"model","thread_id":"sender"}).to_string(),
        );
        add(
            "gc-question",
            "gc-target",
            &json!({"kind":"model","thread_id":"sender"}).to_string(),
        );
        add(
            "sender-question",
            "live-target",
            &json!({"kind":"model","thread_id":"deleted-sender"}).to_string(),
        );
        add("cancel-question", "live-target", r#"{"kind":"human"}"#);
        let tx = db.transaction().unwrap();
        deleted_threads_tx(&tx, &["bulk-target".into()], "now", &mut Vec::new()).unwrap();
        deleted_recipient_receipt_tx(&tx, "gc-question", "now", &mut Vec::new()).unwrap();
        deleted_sender_receipt_tx(&tx, "sender-question", "now", &mut Vec::new()).unwrap();
        assert!(
            cancel_reply_tx(
                &tx,
                "live-target",
                "cancel-question",
                "now",
                &json!({"kind":"human"}),
                false
            )
            .unwrap()
        );
        tx.commit().unwrap();
        for message in [
            "bulk-question",
            "gc-question",
            "sender-question",
            "cancel-question",
        ] {
            let reply = named_reply_state(&db, message);
            let admission: String = db
                .query_row(
                    "SELECT path_admission FROM receipt WHERE message_node_id=?1",
                    [message],
                    |row| row.get(0),
                )
                .unwrap();
            let expected = match message {
                "bulk-question" | "gc-question" => "recipient_deleted",
                "sender-question" | "cancel-question" => "cancelled",
                _ => unreachable!(),
            };
            assert_eq!(reply, expected, "{message}");
            assert_eq!(
                admission, "unadmitted",
                "terminal reply does not create path admission for {message}"
            );
        }
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
            complete_tx(&tx, "t", "r", "one", &mut Vec::new(), &mut Vec::new())
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
            complete_tx(&tx, "t", "r", "two", &mut Vec::new(), &mut Vec::new())
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
            complete_tx(
                &tx,
                "t",
                "new-run",
                "three",
                &mut Vec::new(),
                &mut Vec::new()
            )
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
        assert_eq!(named_reply_state(&db, "m"), "ignored");
    }
    #[test]
    fn path_admission_remains_transactional_without_notice_events() {
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
        assert_eq!(path, ["a", "b"]);
        super::super::message_delivery::mark_admitted_tx(
            &tx,
            "t",
            &["a".into(), "b".into()],
            "now",
        )
        .unwrap();
        assert_eq!(
            tx.query_row(
                "SELECT count(*) FROM receipt WHERE path_admission='admitted'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            tx.query_row("SELECT count(*) FROM event", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            0
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
