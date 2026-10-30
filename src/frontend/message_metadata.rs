//! Bounded saved source facts for presentation, never inferred from rendered roles.
use crate::{
    control::Control,
    presentation::InputSource,
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
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
struct RawSource {
    node_id: String,
    created_at: Option<String>,
    revisit: bool,
    // Preserve raw legacy null separately from missing/ambiguous evidence.
    // No sender JSON is interpreted on the DB worker.
    sender: Option<String>,
}
pub(super) struct SourceFacts {
    viewed: Option<String>,
    parent: Option<String>,
    rows: Vec<RawSource>,
}

impl SourceFacts {
    /// Merge bounded sets captured in the same current-view transaction.
    #[cfg(test)]
    pub(super) fn append(&mut self, other: Self) -> Result<(), StorageError> {
        if self.viewed != other.viewed || self.parent != other.parent {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Source metadata scopes do not match",
            )
            .into());
        }
        let mut seen = self
            .rows
            .iter()
            .map(|row| row.node_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let added = other
            .rows
            .into_iter()
            .filter(|row| seen.insert(row.node_id.clone()))
            .collect::<Vec<_>>();
        if self.rows.len() + added.len() > 2 * MAX_NODES {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "Combined source metadata exceeds its bounded sets",
            )
            .into());
        }
        self.rows.extend(added);
        Ok(())
    }
}

pub(super) fn read_tx(
    db: &Connection,
    viewed: Option<&str>,
    ids: &[String],
) -> Result<SourceFacts, StorageError> {
    if ids.len() > MAX_NODES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Display source metadata exceeds its node bound",
        )
        .into());
    }
    let parent: Option<String> = match viewed {
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
    let rows = ids
        .iter()
        .map(|id| read_row(db, id, viewed))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SourceFacts {
        viewed: viewed.map(str::to_owned),
        parent,
        rows,
    })
}

pub(super) fn apply(nodes: &mut [UiNode], facts: &SourceFacts) {
    let keyed = facts
        .rows
        .iter()
        .map(|row| (row.node_id.as_str(), row))
        .collect::<std::collections::HashMap<_, _>>();
    for node in nodes {
        let Some(raw) = keyed.get(node.node_id.as_str()) else {
            continue;
        };
        let metadata = project(raw, facts.viewed.as_deref(), facts.parent.as_deref());
        node.presentation.created_at = metadata.created_at;
        node.presentation.input_source = metadata.source;
        node.presentation.bound_metadata();
    }
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
    let facts = control
        .store
        .database
        .call(move |db| read_tx(db, viewed.as_deref(), &ids))
        .await
        .map_err(StorageError::into_api_error)?;
    let owned = nodes.iter_mut().map(std::mem::take).collect::<Vec<_>>();
    let projected = super::presentation_work(move || {
        let mut owned = owned;
        apply(&mut owned, &facts);
        owned
    })
    .await?;
    for (target, node) in nodes.iter_mut().zip(projected) {
        *target = node;
    }
    Ok(())
}

fn read_row(db: &Connection, id: &str, viewed: Option<&str>) -> Result<RawSource, StorageError> {
    let row: Option<(String,String,Option<String>,Option<String>)> = db.query_row(
        "SELECT n.created_at,n.kind,u.thread_id,CASE WHEN json_valid(n.payload_json) THEN json_extract(n.payload_json,'$.event_id') END FROM node n LEFT JOIN turn u ON u.id=n.turn_id WHERE n.id=?1",
        [id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).optional()?;
    let mut raw = RawSource {
        node_id: id.into(),
        created_at: None,
        revisit: false,
        sender: None,
    };
    let Some((stamp, kind, source_thread, event)) = row else {
        return Ok(raw);
    };
    raw.created_at = (stamp.len() <= 128).then(|| stamp.clone());
    let target = source_thread.as_deref().or(viewed);
    if kind == "event"
        && let (Some(event), Some(target)) = (event, target)
    {
        raw.revisit=db.query_row("SELECT EXISTS(SELECT 1 FROM event WHERE id=?1 AND kind='revisit' AND target_thread_id=?2)",params![event,target],|row|row.get(0))?;
        if raw.revisit {
            return Ok(raw);
        }
    }
    if !matches!(kind.as_str(), "user" | "user_message" | "message") {
        return Ok(raw);
    }
    let Some(target) = target else {
        return Ok(raw);
    };
    // Same finite receipt-target/state/stamp seeks as the ordinary wrapper.
    let mut matches = Vec::new();
    let mut query = db.prepare_cached(RECEIPTS_AT_STAMP)?;
    for state in ["accepted", "held", "applied", "cancelled"] {
        let candidates = query
            .query_map(params![target, state, stamp], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if candidates.len() > STAMP_CANDIDATES {
            return Ok(raw);
        }
        matches.extend(
            candidates
                .into_iter()
                .filter(|(node, _)| node == id)
                .map(|(_, sender)| sender),
        );
    }
    if matches.len() == 1 {
        raw.sender = matches.pop();
    }
    Ok(raw)
}

fn project(raw: &RawSource, viewed: Option<&str>, parent: Option<&str>) -> Metadata {
    let source = if raw.revisit {
        InputSource::Revisit
    } else {
        match raw
            .sender
            .as_deref()
            .and_then(|sender| serde_json::from_str::<serde_json::Value>(sender).ok())
        {
            Some(serde_json::Value::Null) => InputSource::Human,
            Some(sender)
                if sender.get("kind").and_then(serde_json::Value::as_str) == Some("human") =>
            {
                InputSource::Human
            }
            Some(sender) => match sender.get("thread_id").and_then(serde_json::Value::as_str) {
                Some(sender) if Some(sender) == viewed => InputSource::SelfModel,
                Some(sender) if Some(sender) == parent => InputSource::Parent,
                Some(_) => InputSource::OtherModel,
                None => InputSource::Unknown,
            },
            None => InputSource::Unknown,
        }
    };
    Metadata {
        created_at: raw.created_at.clone(),
        source,
    }
}

#[cfg(test)]
fn read(
    db: &Connection,
    id: &str,
    viewed: Option<&str>,
    parent: Option<&str>,
) -> Result<Metadata, StorageError> {
    Ok(project(&read_row(db, id, viewed)?, viewed, parent))
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
                "recorded-human",
                Some(r#"{"kind":"human"}"#),
                "applied",
                InputSource::Human,
            ),
            (
                "named-external",
                Some(r#"{"kind":"external_cli","label":"human"}"#),
                "applied",
                InputSource::Unknown,
            ),
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
    #[test]
    fn raw_receipt_evidence_survives_db_exit_and_keeps_59_null_and_label_boundaries() {
        let db = fixture();
        input(&db, "legacy-null", Some("null"), "applied");
        input(
            &db,
            "named-cli",
            Some(r#"{"kind":"external_cli","label":"human"}"#),
            "applied",
        );
        let mut facts = read_tx(&db, Some("child"), &["legacy-null".into()]).unwrap();
        assert_eq!(facts.rows[0].sender.as_deref(), Some("null"));
        facts
            .append(read_tx(&db, Some("child"), &["named-cli".into()]).unwrap())
            .unwrap();
        facts
            .append(read_tx(&db, Some("child"), &["legacy-null".into()]).unwrap())
            .unwrap();
        assert_eq!(facts.rows.len(), 2);
        assert!(
            facts
                .append(read_tx(&db, Some("fork"), &["legacy-null".into()]).unwrap())
                .is_err()
        );
        drop(db);
        let mut nodes = ["named-cli", "legacy-null"]
            .into_iter()
            .map(|id| UiNode {
                node_id: id.into(),
                presentation: crate::presentation::NodePresentation {
                    read_revision: 77,
                    body_evicted: true,
                    ..Default::default()
                },
                ..Default::default()
            })
            .collect::<Vec<_>>();
        apply(&mut nodes, &facts);
        assert_eq!(
            nodes[0].presentation.input_source,
            InputSource::Unknown,
            "a self-asserted human label never changes CLI authority"
        );
        assert_eq!(
            nodes[1].presentation.input_source,
            InputSource::Human,
            "historical null retains only its band convention"
        );
        assert_eq!(
            crate::protocol::MessageSender::from_saved(Some("null")).kind,
            crate::protocol::SenderKind::Unknown,
            "explicit sender inspection remains unknown for legacy null"
        );
        assert!(nodes.iter().all(|node|node.presentation.read_revision==77 && node.presentation.body_evicted));
    }

    #[test]
    fn separate_bounded_source_sets_merge_above_128_without_silent_truncation() {
        let db = fixture();
        let first = (0..128).map(|n| format!("missing-{n}")).collect::<Vec<_>>();
        let second = (128..178)
            .map(|n| format!("missing-{n}"))
            .collect::<Vec<_>>();
        let mut facts = read_tx(&db, Some("child"), &first).unwrap();
        facts
            .append(read_tx(&db, Some("child"), &second).unwrap())
            .unwrap();
        assert_eq!(facts.rows.len(), 178);
        assert_eq!(
            read_tx(&db, Some("child"), &vec!["missing".into(); 129])
                .err()
                .unwrap()
                .into_api_error()
                .code,
            ErrorCode::InputLimit
        );
    }
}
