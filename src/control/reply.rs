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
        let sender: Value = serde_json::from_str(&sender)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
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
