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
        None
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

pub(crate) fn awaiting_reply_tx(db: &Connection, thread: &str) -> rusqlite::Result<Vec<Value>> {
    let mut statement = db.prepare("SELECT message_node_id,sender_json,created_at,state,result_json FROM receipt WHERE target_thread_id=?1 AND json_extract(result_json,'$.reply.state')='pending' ORDER BY created_at,rowid LIMIT 128")?;
    statement.query_map([thread], |r| {
        let saved: String = r.get(4)?;
        let saved: Value = serde_json::from_str(&saved).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let sender: String = r.get(1)?;
        Ok(json!({"message_id":r.get::<_,String>(0)?,"sender":crate::protocol::MessageSender::from_saved(Some(&sender)),"created_at":r.get::<_,String>(2)?,"delivery_status":r.get::<_,String>(3)?,"reminded":saved["reply"].get("reminded_at").is_some()}))
    })?.collect()
}

pub(crate) fn cancel_reply_tx(
    db: &Connection,
    thread: &str,
    message: &str,
    at: &str,
    actor: &Value,
    admitted: bool,
) -> rusqlite::Result<bool> {
    let changed = db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply',json(?2)),updated_at=?3 WHERE message_node_id=?1 AND json_extract(result_json,'$.reply.state')='pending'",params![message,json!({"state":"cancelled","at":at,"by":actor}).to_string(),at])?;
    if changed == 1 && admitted {
        // Leave the current selected path and inflight manifest intact. The
        // ordinary event admission path presents this notice without waking a Run.
        db.execute(
            "INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at) VALUES(?1,'context_changed','{\"kind\":\"shellboy\"}',?2,?3,?4)",
            params![format!("reply-cancelled-{message}"), thread, json!({"kind":"reply_cancelled","message_id":message}).to_string(), at],
        )?;
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
        if caller.is_some_and(|c| c.thread_id != row.0) {
            return Err(
                ApiError::new(ErrorCode::InvalidState, "only the recipient may reply")
                    .details(json!({"reason":"not_recipient"})),
            );
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

#[cfg(test)]
mod tests {
    use super::*;
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
