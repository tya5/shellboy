//! Fail-closed saved-fact validation; emission additionally requires response budgeting.
use crate::protocol::{MessageAccepted, MessageProvenance, MessageReplyState};
use serde::Deserialize;
use serde_json::Value;

// Match the existing saved outcome ceiling, before allocating/parsing receipt JSON.
const MAX_RECEIPT_BYTES: usize = 8 * 1024 * 1024;

pub(super) const REVERSE_LOOKUP: &str = "SELECT id,message_node_id FROM receipt INDEXED BY receipt_reply_node_lookup WHERE target_thread_id=?1 AND (CASE WHEN json_valid(result_json) THEN CASE WHEN json_type(result_json,'$.reply.node_id')='text' THEN json_extract(result_json,'$.reply.node_id') END END)=?2 LIMIT 2";

fn id(value: &str, source: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && value != source
}

pub(super) fn saved_relation(raw: &str, body: &str, thread: &str) -> Option<MessageProvenance> {
    if raw.len() > MAX_RECEIPT_BYTES {
        return None;
    }
    let mut parser = serde_json::Deserializer::from_str(raw);
    // Keep serde_json's default depth limit enabled for legacy/corrupt receipts.
    let value = Value::deserialize(&mut parser).ok()?;
    parser.end().ok()?;
    let accepted: MessageAccepted = serde_json::from_value(value.clone()).ok()?;
    accepted.validate().ok()?;
    if accepted.message_id != body || accepted.thread_id != thread {
        return None;
    }
    let mut relation = MessageProvenance {
        expect_reply: accepted.expect_reply,
        reply_to: None,
        reply_state: None,
        reply_node_id: None,
    };
    if let Some(reply_to) = value.get("reply_to") {
        let target = reply_to.as_str()?;
        if accepted.expect_reply || value.get("reply").is_some() || !id(target, body) {
            return None;
        }
        relation.reply_to = Some(target.into());
    } else if accepted.expect_reply {
        let reply = value.get("reply")?.as_object()?;
        let state = reply.get("state")?.as_str()?;
        relation.reply_state = Some(match state {
            "pending" => MessageReplyState::Pending,
            "replied" => MessageReplyState::Replied,
            "ignored" => MessageReplyState::Ignored,
            "cancelled" => MessageReplyState::Cancelled,
            _ => return None,
        });
        if state == "replied" {
            let target = reply.get("node_id")?.as_str()?;
            if !id(target, body) {
                return None;
            }
            relation.reply_node_id = Some(target.into());
        } else if reply.contains_key("node_id") {
            return None;
        }
    } else if value.get("reply").is_some() {
        return None;
    }
    (serde_json::to_vec(&relation).ok()?.len() <= 4096).then_some(relation)
}

/// Check the byte length in SQLite before returning any owned result string.
pub(super) fn bounded_receipt(
    db: &rusqlite::Connection,
    receipt: &str,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    db.query_row("SELECT CASE WHEN length(CAST(result_json AS BLOB))<=?2 THEN result_json END FROM receipt WHERE id=?1", rusqlite::params![receipt, MAX_RECEIPT_BYTES as i64], |row|row.get(0)).optional().map(Option::flatten)
}

/// Count identities first: an oversized candidate still makes two receipts ambiguous.
pub(super) fn unique_reverse(
    db: &rusqlite::Connection,
    thread: &str,
    node: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    let mut statement = db.prepare(REVERSE_LOOKUP)?;
    let mut rows = statement.query([thread, node])?;
    let Some(first) = rows.next()? else {
        return Ok(None);
    };
    // Inspect borrowed SQLite bytes before allocating identity strings.
    let receipt = match first.get_ref(0)? {
        rusqlite::types::ValueRef::Text(bytes) if bytes.len() <= 256 => {
            match std::str::from_utf8(bytes) {
                Ok(id) if !id.trim().is_empty() => id.to_owned(),
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    let body: Option<String> = match first.get_ref(1)? {
        rusqlite::types::ValueRef::Text(bytes) if bytes.len() <= 256 => {
            std::str::from_utf8(bytes).ok().map(str::to_owned)
        }
        _ => None,
    };
    if rows.next()?.is_some() {
        return Ok(None);
    }
    drop(rows);
    drop(statement);
    let Some(body) = body else {
        return Ok(None);
    };
    Ok(bounded_receipt(db, &receipt)?.map(|raw| (body, raw)))
}

/// Relate only receipts proved by durable node identity and its creation Thread.
pub(super) fn project(
    db: &rusqlite::Connection,
    node: &str,
    thread: &str,
) -> rusqlite::Result<Option<MessageProvenance>> {
    use rusqlite::OptionalExtension;
    let own_row: Option<Option<String>> = db
        .query_row(
            "SELECT CASE WHEN length(CAST(id AS BLOB))<=256 THEN id END FROM receipt WHERE message_node_id=?1 AND target_thread_id=?2",
            [node, thread],
            |r| r.get::<_,Option<String>>(0),
        )
        .optional()?;
    let own_present = own_row.is_some();
    let own_relation = if let Some(Some(receipt)) = own_row {
        bounded_receipt(db, &receipt)?.and_then(|raw| saved_relation(&raw, node, thread))
    } else {
        None
    };
    if let Some(relation) = own_relation.as_ref()
        && let Some(target) = relation
            .reply_to
            .as_deref()
            .or(relation.reply_node_id.as_deref())
    {
        let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM node n JOIN turn t ON t.id=n.turn_id WHERE n.id=?1 AND t.thread_id=?2)",[target,thread],|r|r.get(0))?;
        if !exists {
            return Ok(None);
        }
    }
    // Count two candidates before reading their JSON; corrupt/oversized rows remain ambiguous.
    let mut statement = db.prepare(REVERSE_LOOKUP)?;
    let mut rows = statement.query([thread, node])?;
    let first = rows.next()?.is_some();
    let ambiguous = first && rows.next()?.is_some();
    drop(rows);
    drop(statement);
    if ambiguous {
        return Ok(None);
    }
    let Some((request, raw)) = unique_reverse(db, thread, node)? else {
        return Ok(if !first { own_relation } else { None });
    };
    if !id(&request, node) {
        return Ok(None);
    }
    let Some(original) = saved_relation(&raw, &request, thread) else {
        return Ok(None);
    };
    if !original.expect_reply || original.reply_node_id.as_deref() != Some(node) {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(&raw).expect("already bounded and parsed");
    let valid_sender = value
        .get("reply")
        .and_then(|r| r.get("by"))
        .and_then(|by| serde_json::from_value::<crate::protocol::MessageSender>(by.clone()).ok())
        .is_some();
    if !valid_sender {
        return Ok(None);
    }
    let request_in_context:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM node n JOIN turn t ON t.id=n.turn_id WHERE n.id=?1 AND t.thread_id=?2)",[request.as_str(),thread],|r|r.get(0))?;
    if !request_in_context {
        return Ok(None);
    }
    if let Some(own) = own_relation {
        if own.expect_reply || own.reply_to.as_deref() != Some(request.as_str()) {
            return Ok(None);
        }
        return Ok(Some(own));
    }
    // A malformed own saved acceptance is not repaired from reverse evidence.
    if own_present {
        return Ok(None);
    }
    Ok(Some(MessageProvenance {
        expect_reply: false,
        reply_to: Some(request),
        reply_state: None,
        reply_node_id: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_plan_scope_before_and_after_reverse_index_on_bounded_distribution() {
        use rusqlite::StatementStatus;
        for full_schema in [false, true] {
            let db = rusqlite::Connection::open_in_memory().unwrap();
            if full_schema {
                db.execute_batch(include_str!("../storage/current.sql"))
                    .unwrap();
                db.execute_batch("PRAGMA foreign_keys=OFF; DROP INDEX receipt_reply_node_lookup; DELETE FROM schema_migration WHERE version=41; PRAGMA user_version=40;").unwrap();
            } else {
                db.execute_batch("CREATE TABLE schema_migration(version INTEGER PRIMARY KEY,name TEXT); CREATE TABLE receipt(id TEXT PRIMARY KEY,target_thread_id TEXT,message_node_id TEXT,result_json TEXT); CREATE INDEX receipt_reply_pending ON receipt(target_thread_id) WHERE json_extract(result_json,'$.reply.state')='pending';").unwrap();
            }
            for i in 0..512 {
                let thread = if i < 256 { "t" } else { "other" };
                let state = if i % 128 == 0 { "pending" } else { "replied" };
                let raw =
                    serde_json::json!({"reply":{"state":state,"node_id":format!("reply{i}")}})
                        .to_string();
                db.execute(
                if full_schema {"INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json) VALUES(?1,?1,'{}',?2,?3,'accepted','now',?4)"} else {"INSERT INTO receipt VALUES(?1,?2,?3,?4)"},
                rusqlite::params![format!("r{i}"), thread, format!("body{i}"), raw],
            )
            .unwrap();
            }
            let measure = |query: &str| {
                let plan = db
                    .query_row(&format!("EXPLAIN QUERY PLAN {query}"), [], |r| {
                        r.get::<_, String>(3)
                    })
                    .unwrap();
                let mut stmt = db.prepare(query).unwrap();
                let values = stmt
                    .query_map([], |r| r.get::<_, i64>(0))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                (plan, stmt.get_status(StatementStatus::VmStep), values)
            };
            let query = "SELECT rowid FROM receipt WHERE target_thread_id='t' AND json_extract(result_json,'$.reply.state')='pending'";
            let before = measure(query);
            db.execute_batch(include_str!(
                "../../migrations/0041_receipt_reply_node_lookup.sql"
            ))
            .unwrap();
            let after = measure(query);
            let forced = measure(
                "SELECT rowid FROM receipt INDEXED BY receipt_reply_pending WHERE target_thread_id='t' AND json_extract(result_json,'$.reply.state')='pending'",
            );
            assert_eq!(before.2, after.2);
            assert_eq!(before.2, forced.2);
            eprintln!(
                "PENDING_SCOPE full_schema={full_schema} rows=512 target_rows=256 target_pending=2 before={:?} after={:?} forced={:?}",
                (before.0, before.1),
                (after.0, after.1),
                (forced.0, forced.1)
            );
            assert!(before.1 > 0 && after.1 > 0 && forced.1 > 0);
        }
    }
    #[test]
    fn strict_states_conflicts_and_depth_are_fail_closed() {
        let accepted = serde_json::json!({"message_id":"body","thread_id":"thread","delivery_status":"accepted","assignment":"pending","expect_reply":true,"reply":{"state":"pending"}});
        assert!(saved_relation(&accepted.to_string(), "body", "thread").is_some());
        for reply in [
            serde_json::json!({"state":"unknown"}),
            serde_json::json!({"state":"pending","node_id":null}),
            serde_json::json!({"state":"replied","node_id":"body"}),
            serde_json::json!({"state":"replied","node_id":"x".repeat(257)}),
        ] {
            let mut bad = accepted.clone();
            bad["reply"] = reply;
            assert!(saved_relation(&bad.to_string(), "body", "thread").is_none());
        }
        assert!(saved_relation(&accepted.to_string(), "foreign", "thread").is_none());
        assert!(saved_relation(&accepted.to_string(), "body", "foreign").is_none());
        assert!(
            saved_relation(
                &format!("{}0{}", "[".repeat(129), "]".repeat(129)),
                "body",
                "thread"
            )
            .is_none()
        );
    }
    #[test]
    fn oversized_second_reverse_candidate_remains_ambiguous() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE schema_migration(version INTEGER PRIMARY KEY,name TEXT); CREATE TABLE receipt(id TEXT PRIMARY KEY,target_thread_id TEXT,message_node_id TEXT,result_json TEXT);").unwrap();
        db.execute_batch(include_str!(
            "../../migrations/0041_receipt_reply_node_lookup.sql"
        ))
        .unwrap();
        db.execute(
            "INSERT INTO receipt VALUES('one','t','request',?1)",
            [r#"{"reply":{"node_id":"reply"}}"#],
        )
        .unwrap();
        assert!(unique_reverse(&db, "t", "reply").unwrap().is_some());
        let large=serde_json::json!({"reply":{"node_id":"reply"},"padding":"x".repeat(MAX_RECEIPT_BYTES)}).to_string();
        db.execute(
            "INSERT INTO receipt VALUES('two','t','request',?1)",
            [large],
        )
        .unwrap();
        assert!(unique_reverse(&db, "t", "reply").unwrap().is_none());
    }
    #[test]
    fn sqlite_materialization_is_byte_bounded_and_reverse_search_uses_both_keys() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE schema_migration(version INTEGER PRIMARY KEY,name TEXT); CREATE TABLE receipt(id TEXT PRIMARY KEY,target_thread_id TEXT,message_node_id TEXT,result_json TEXT);").unwrap();
        db.execute("INSERT INTO receipt VALUES('bad','t','b','{broken')", [])
            .unwrap();
        db.execute_batch(include_str!(
            "../../migrations/0041_receipt_reply_node_lookup.sql"
        ))
        .unwrap();
        db.execute(
            "INSERT INTO receipt VALUES('large','t','b',?1)",
            ["x".repeat(MAX_RECEIPT_BYTES + 1)],
        )
        .unwrap();
        assert!(bounded_receipt(&db, "large").unwrap().is_none());
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {REVERSE_LOOKUP}"))
            .unwrap()
            .query_map(["t", "b"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" ");
        assert!(
            plan.contains("SEARCH")
                && plan.contains("target_thread_id=?")
                && plan.contains("<expr>=?"),
            "{plan}"
        );
    }
}
