//! Bounded saved source facts for presentation, never inferred from rendered roles.
use crate::{
    control::Control,
    presentation::InputSource,
    protocol::{ApiError, ErrorCode},
    ui::UiNode,
};
use rusqlite::{Connection, OptionalExtension, params};

const MAX_NODES: usize = 128;
const STAMP_CANDIDATES: usize = 128;
const RECEIPTS_AT_STAMP: &str = "SELECT message_node_id,substr(sender_json,1,8193) FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state=?2 AND created_at=?3 LIMIT 129";

struct Metadata {
    created_at: Option<String>,
    source: InputSource,
}

pub(super) async fn enrich(
    control: &Control,
    viewed_thread: Option<&str>,
    nodes: &mut [UiNode],
) -> Result<(), ApiError> {
    if nodes.len() > MAX_NODES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Display source metadata exceeds its node bound",
        ));
    }
    let ids = nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    let viewed = viewed_thread.map(str::to_owned);
    let metadata = control
        .store
        .database
        .call(move |db| {
            let parent: Option<String> = match viewed.as_deref() {
                Some(thread) => db
                    .query_row(
                        "SELECT parent_thread_id FROM thread WHERE id=?1",
                        [thread],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten(),
                None => None,
            };
            ids.iter()
                .map(|id| read(db, id, viewed.as_deref(), parent.as_deref()))
                .collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|error| error.into_api_error())?;
    for (node, metadata) in nodes.iter_mut().zip(metadata) {
        node.presentation.created_at = metadata.created_at;
        node.presentation.input_source = metadata.source;
        node.presentation.bound_metadata();
    }
    Ok(())
}

fn read(
    db: &Connection,
    id: &str,
    viewed: Option<&str>,
    parent: Option<&str>,
) -> Result<Metadata, crate::storage::StorageError> {
    let row: Option<(String,String,Option<String>,Option<String>)> = db.query_row(
        "SELECT n.created_at,n.kind,u.thread_id,CASE WHEN json_valid(n.payload_json) THEN json_extract(n.payload_json,'$.event_id') END FROM node n LEFT JOIN turn u ON u.id=n.turn_id WHERE n.id=?1",
        [id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).optional()?;
    let Some((stamp, kind, source_thread, event)) = row else {
        return Ok(Metadata {
            created_at: None,
            source: InputSource::Unknown,
        });
    };
    let created_at = (stamp.len() <= 128).then(|| stamp.clone());
    let target = source_thread.as_deref().or(viewed);
    if kind == "event"
        && let (Some(event), Some(target)) = (event, target)
    {
        let revisit: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM event WHERE id=?1 AND kind='revisit' AND target_thread_id=?2)",params![event,target],|row|row.get(0))?;
        if revisit {
            return Ok(Metadata {
                created_at,
                source: InputSource::Revisit,
            });
        }
    }
    if !matches!(kind.as_str(), "user" | "user_message" | "message") {
        return Ok(Metadata {
            created_at,
            source: InputSource::Unknown,
        });
    }
    let Some(target) = target else {
        return Ok(Metadata {
            created_at,
            source: InputSource::Unknown,
        });
    };
    // There is no receipt-by-node index. Four exact target/state/stamp seeks
    // inspect at most129 rows each. Timestamp collisions fail closed; never
    // scan a Thread or the global receipt table to guess sender identity.
    let mut matches = Vec::new();
    let mut query = db.prepare_cached(RECEIPTS_AT_STAMP)?;
    for state in ["accepted", "held", "applied", "cancelled"] {
        let candidates = query
            .query_map(params![target, state, stamp], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if candidates.len() > STAMP_CANDIDATES {
            return Ok(Metadata {
                created_at,
                source: InputSource::Unknown,
            });
        }
        matches.extend(
            candidates
                .into_iter()
                .filter(|(node, _)| node == id)
                .map(|(_, sender)| sender),
        );
    }
    let source = if matches.len() != 1 {
        InputSource::Unknown
    } else {
        match serde_json::from_str::<serde_json::Value>(&matches[0]).ok() {
            Some(serde_json::Value::Null) => InputSource::Human,
            Some(sender) => match sender.get("thread_id").and_then(serde_json::Value::as_str) {
                Some(sender) if Some(sender) == viewed => InputSource::SelfModel,
                Some(sender) if Some(sender) == parent => InputSource::Parent,
                Some(_) => InputSource::OtherModel,
                None => InputSource::Unknown,
            },
            None => InputSource::Unknown,
        }
    };
    Ok(Metadata { created_at, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','/','/a','p','m','0'),('child','/','/a','p','m','0'),('fork','/','/a','p','m','0'); UPDATE thread SET parent_thread_id='parent' WHERE id='child'; INSERT INTO run(id,thread_id,state,created_at)VALUES('r','child','completed','0'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','child','r','completed','0');").unwrap();
        db
    }
    fn input(db: &Connection, id: &str, sender: Option<&str>, state: &str) {
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','user',?1,'{}')",[id]).unwrap();
        if let Some(sender) = sender {
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at)VALUES(?1,?1,?2,'child',?1,'r','u',?3,?1)",params![id,sender,state]).unwrap();
        }
    }
    #[test]
    fn sender_requires_saved_receipt_and_uses_view_relative_parent_and_self() {
        let db = fixture();
        for (id, sender, state, expected) in [
            ("human", Some("null"), "applied", InputSource::Human),
            (
                "parent",
                Some("{\"thread_id\":\"parent\"}"),
                "accepted",
                InputSource::Parent,
            ),
            (
                "self",
                Some("{\"thread_id\":\"child\"}"),
                "held",
                InputSource::SelfModel,
            ),
            (
                "other",
                Some("{\"thread_id\":\"elsewhere\"}"),
                "cancelled",
                InputSource::OtherModel,
            ),
            ("missing", None, "applied", InputSource::Unknown),
            ("bad", Some("{}"), "applied", InputSource::Unknown),
        ] {
            input(&db, id, sender, state);
            let meta = read(&db, id, Some("child"), Some("parent")).unwrap();
            assert_eq!(meta.source, expected);
            assert_eq!(meta.created_at.as_deref(), Some(id));
        }
        // A fork's selected nodes still locate the original receipt by source
        // Turn, but sender classification is relative to the viewed Thread.
        assert_eq!(
            read(&db, "self", Some("fork"), None).unwrap().source,
            InputSource::OtherModel
        );
        db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('unassigned','user','stamp','{}')",[]).unwrap();
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES('unassigned','unassigned','null','child','unassigned','held','stamp')",[]).unwrap();
        assert_eq!(
            read(&db, "unassigned", Some("child"), None).unwrap().source,
            InputSource::Human
        );
        assert_eq!(
            read(&db, "unassigned", None, None).unwrap().source,
            InputSource::Unknown
        );
    }
    #[test]
    fn revisit_requires_saved_event_relation_and_collisions_are_bounded_unknown() {
        let db = fixture();
        db.execute_batch("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('event','revisit','{}','child','{}','time'); INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('revisit','u','event','time','{\"event_id\":\"event\"}'),('fake','u','event','time','{\"kind\":\"revisit\"}');").unwrap();
        assert_eq!(
            read(&db, "revisit", Some("fork"), None).unwrap().source,
            InputSource::Revisit
        );
        assert_eq!(
            read(&db, "fake", Some("child"), None).unwrap().source,
            InputSource::Unknown
        );
        for i in 0..=STAMP_CANDIDATES {
            let id = format!("collision-{i}");
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','user','collision','{}')",[&id]).unwrap();
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES(?1,?1,'null','child',?1,'applied','collision')",[&id]).unwrap();
        }
        assert_eq!(
            read(&db, "collision-0", Some("child"), None)
                .unwrap()
                .source,
            InputSource::Unknown
        );
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {RECEIPTS_AT_STAMP}"))
            .unwrap()
            .query_map(params!["child", "applied", "collision"], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(plan.contains("receipt_target"), "{plan}");
        assert!(plan.contains("created_at=?"), "{plan}");
        assert!(RECEIPTS_AT_STAMP.ends_with("LIMIT 129"));
    }
}
