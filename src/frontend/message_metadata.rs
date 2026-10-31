//! Bounded saved source facts for presentation, never inferred from rendered roles.
use crate::{
    control::Control,
    presentation::InputSource,
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
    ui::UiNode,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, HashMap};

const MAX_NODES: usize = 128;
const STAMP_CANDIDATES: usize = 128;
// Each recursive chain seeks one exact indexed target/state/stamp range. Its
// 129th witness proves overflow before requested-node matching or sender copies.
const RECEIPTS_AT_STAMP: &str = "WITH RECURSIVE
keys AS MATERIALIZED (
    SELECT CAST(j.key AS INTEGER) AS bucket,json_extract(j.value,'$.target') AS target,
        json_extract(j.value,'$.stamp') AS stamp FROM json_each(?1) j),
states(state) AS (VALUES('accepted'),('held'),('applied'),('cancelled')),
candidates(bucket,state,receipt_id,depth) AS (
    SELECT b.bucket,s.state,(SELECT r.rowid FROM receipt r INDEXED BY receipt_target
        WHERE r.target_thread_id=b.target AND r.state=s.state AND r.created_at=b.stamp
        ORDER BY r.rowid LIMIT 1),1 FROM keys b CROSS JOIN states s
    UNION ALL
    SELECT c.bucket,c.state,(SELECT r.rowid FROM receipt r INDEXED BY receipt_target
        WHERE r.target_thread_id=b.target AND r.state=c.state AND r.created_at=b.stamp
            AND r.rowid>c.receipt_id ORDER BY r.rowid LIMIT 1),c.depth+1
    FROM candidates c JOIN keys b ON b.bucket=c.bucket
    WHERE c.receipt_id IS NOT NULL AND c.depth<129)
SELECT c.bucket,c.state,c.receipt_id,
    CASE WHEN r.message_node_id IN (SELECT value FROM json_each(?2))
        THEN r.message_node_id END
FROM candidates c JOIN receipt r ON r.rowid=c.receipt_id";
const COMMON_SOURCES: &str = "WITH requested AS (SELECT DISTINCT value AS id FROM json_each(?1))
    SELECT q.id,n.created_at,COALESCE(n.kind IN ('user','user_message','message'),0),
        COALESCE(u.thread_id,?2),e.id IS NOT NULL
    FROM requested q LEFT JOIN node n ON n.id=q.id
    LEFT JOIN turn u ON u.id=n.turn_id
    LEFT JOIN event e ON n.kind='event' AND e.id=CASE WHEN json_valid(n.payload_json)
        THEN json_extract(n.payload_json,'$.event_id') END
        AND e.kind='revisit' AND e.target_thread_id=COALESCE(u.thread_id,?2)";

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
    let encoded = serde_json::to_string(ids).expect("String IDs serialize");
    let mut query = db.prepare_cached(COMMON_SOURCES)?;
    let common = query
        .query_map(params![encoded, viewed], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut rows = Vec::with_capacity(common.len());
    let mut buckets = BTreeMap::<(String, String), Vec<usize>>::new();
    for (node_id, stamp, user, target, revisit) in common {
        let index = rows.len();
        if user
            && !revisit
            && let (Some(target), Some(stamp)) = (target, stamp.as_ref())
        {
            buckets
                .entry((target, stamp.clone()))
                .or_default()
                .push(index);
        }
        rows.push(RawSource {
            node_id,
            created_at: stamp.filter(|stamp| stamp.len() <= 128),
            revisit,
            sender: None,
        });
    }
    // All distinct timestamp/state buckets share one bounded indexed query.
    // Only bucket IDs, rowids and matching requested IDs cross the DB boundary.
    let buckets = buckets.into_iter().collect::<Vec<_>>();
    let mut witnesses = Vec::new();
    if !buckets.is_empty() {
        let keys = buckets
            .iter()
            .map(|((target, stamp), _)| serde_json::json!({"target":target,"stamp":stamp}))
            .collect::<Vec<_>>();
        let keys = serde_json::to_string(&keys).expect("Receipt keys serialize");
        let mut counts = vec![[0_usize; 4]; buckets.len()];
        let mut matched = vec![HashMap::<String, Vec<i64>>::new(); buckets.len()];
        let mut node_buckets = HashMap::new();
        for (bucket, (_, indices)) in buckets.iter().enumerate() {
            for &index in indices {
                node_buckets.insert(rows[index].node_id.as_str(), bucket);
            }
        }
        let mut receipts = db.prepare_cached(RECEIPTS_AT_STAMP)?;
        let candidates = receipts.query_map(params![keys, encoded], |row| {
            Ok((
                row.get::<_, i64>(0)? as usize,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for candidate in candidates {
            let (bucket, state, receipt, node) = candidate?;
            let state_index = match state.as_str() {
                "accepted" => 0,
                "held" => 1,
                "applied" => 2,
                "cancelled" => 3,
                _ => unreachable!("receipt states come from fixed VALUES"),
            };
            counts[bucket][state_index] += 1;
            if let Some(node) = node
                && node_buckets.get(node.as_str()) == Some(&bucket)
            {
                let matches = matched[bucket].entry(node).or_default();
                if matches.len() < 2 {
                    matches.push(receipt);
                }
            }
        }
        for (bucket, (_, indices)) in buckets.iter().enumerate() {
            if counts[bucket].iter().any(|&count| count > STAMP_CANDIDATES) {
                continue;
            }
            for &index in indices {
                if let Some(matches) = matched[bucket].get(&rows[index].node_id)
                    && let [receipt] = matches.as_slice()
                {
                    witnesses.push((index, *receipt));
                }
            }
        }
    }
    if !witnesses.is_empty() {
        let receipt_ids = witnesses
            .iter()
            .map(|(_, receipt)| *receipt)
            .collect::<Vec<_>>();
        let encoded = serde_json::to_string(&receipt_ids).expect("Receipt rowids serialize");
        let mut senders = db.prepare_cached("SELECT r.rowid,substr(r.sender_json,1,8193) FROM json_each(?1) q JOIN receipt r ON r.rowid=q.value")?;
        let mut senders = senders
            .query_map([encoded], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?;
        for (index, receipt) in witnesses {
            rows[index].sender = senders.remove(&receipt);
        }
    }
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
    let facts = read_tx(db, viewed, &[id.into()])?;
    Ok(project(&facts.rows[0], viewed, parent))
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
    fn common_tool_metadata_uses_fixed_batch_queries_and_deduplicates_ids() {
        let db = fixture();
        let mut ids = Vec::new();
        for ordinal in 0..50 {
            let id = format!("tool-{ordinal}");
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','tool_result','saved-time','{}')", [&id]).unwrap();
            ids.push(id);
        }
        ids.extend(["tool-0".into(), "tool-49".into()]);
        let statements = crate::frontend::sql_trace::Statements::new(&db);
        let facts = read_tx(&db, Some("child"), &ids).unwrap();
        assert_eq!(
            statements.count(),
            2,
            "one scope query and one node/turn/event batch; no receipt reads for tools"
        );
        assert_eq!(facts.rows.len(), 50);
        assert!(
            facts
                .rows
                .iter()
                .all(|row| row.created_at.as_deref() == Some("saved-time")
                    && row.sender.is_none()
                    && !row.revisit)
        );
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {COMMON_SOURCES}"))
            .unwrap()
            .query_map(
                params![serde_json::to_string(&ids).unwrap(), "child"],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(
            plan.contains("SEARCH n") && plan.contains("SEARCH u") && plan.contains("SEARCH e"),
            "{plan}"
        );
        assert!(
            !plan.contains("SCAN n") && !plan.contains("SCAN e"),
            "{plan}"
        );
    }

    #[test]
    fn mixed_batch_shares_receipt_buckets_without_losing_sender_or_revisit_evidence() {
        let db = fixture();
        for (id, sender, state) in [
            ("human", Some("null"), "applied"),
            (
                "parent-input",
                Some(r#"{"thread_id":"parent"}"#),
                "accepted",
            ),
            ("self-input", Some(r#"{"thread_id":"child"}"#), "held"),
            (
                "other-input",
                Some(r#"{"thread_id":"elsewhere"}"#),
                "cancelled",
            ),
            (
                "external",
                Some(r#"{"kind":"external_cli","label":"human"}"#),
                "applied",
            ),
            ("ambiguous", Some("null"), "applied"),
            ("unreceipted", None, "applied"),
        ] {
            input(&db, id, sender, state);
        }
        db.execute_batch("UPDATE node SET created_at='shared'; UPDATE receipt SET created_at='shared'; INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES('second','second','null','child','ambiguous','held','shared'); INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('event','revisit','{}','child','{}','shared'); INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('revisit','u','event','shared','{\"event_id\":\"event\"}'),('fake','u','event','shared','{\"kind\":\"revisit\"}');").unwrap();
        let ids = [
            "human",
            "parent-input",
            "self-input",
            "other-input",
            "external",
            "ambiguous",
            "unreceipted",
            "revisit",
            "fake",
            "absent",
            "human",
        ]
        .map(str::to_owned);
        let statements = crate::frontend::sql_trace::Statements::new(&db);
        let facts = read_tx(&db, Some("child"), &ids).unwrap();
        assert_eq!(
            statements.count(),
            4,
            "scope+common+one bounded receipt batch+one unique-sender batch"
        );
        assert_eq!(facts.rows.len(), 10);
        for (id, expected) in [
            ("human", InputSource::Human),
            ("parent-input", InputSource::Parent),
            ("self-input", InputSource::SelfModel),
            ("other-input", InputSource::OtherModel),
            ("external", InputSource::Unknown),
            ("ambiguous", InputSource::Unknown),
            ("unreceipted", InputSource::Unknown),
            ("revisit", InputSource::Revisit),
            ("fake", InputSource::Unknown),
            ("absent", InputSource::Unknown),
        ] {
            let row = facts.rows.iter().find(|row| row.node_id == id).unwrap();
            assert_eq!(
                project(row, facts.viewed.as_deref(), facts.parent.as_deref()).source,
                expected,
                "{id}"
            );
            assert_eq!(
                row.created_at.as_deref(),
                (id != "absent").then_some("shared")
            );
        }
        assert_eq!(
            facts
                .rows
                .iter()
                .find(|row| row.node_id == "human")
                .unwrap()
                .sender
                .as_deref(),
            Some("null")
        );
        assert!(
            facts
                .rows
                .iter()
                .find(|row| row.node_id == "ambiguous")
                .unwrap()
                .sender
                .is_none()
        );
        let fork = read_tx(&db, Some("fork"), &["self-input".into()]).unwrap();
        assert_eq!(
            project(
                &fork.rows[0],
                fork.viewed.as_deref(),
                fork.parent.as_deref()
            )
            .source,
            InputSource::OtherModel
        );
    }

    #[test]
    fn distinct_user_timestamps_still_execute_only_four_sql_statements() {
        let db = fixture();
        let ids = (0..50)
            .map(|index| format!("input-{index}"))
            .collect::<Vec<_>>();
        for (index, id) in ids.iter().enumerate() {
            input(
                &db,
                id,
                Some("null"),
                ["accepted", "held", "applied", "cancelled"][index % 4],
            );
        }
        let statements = crate::frontend::sql_trace::Statements::new(&db);
        let facts = read_tx(&db, Some("child"), &ids).unwrap();
        assert_eq!(
            statements.count(),
            4,
            "distinct timestamp buckets remain one receipt batch"
        );
        assert_eq!(facts.rows.len(), 50);
        assert!(
            facts
                .rows
                .iter()
                .all(|row| row.sender.as_deref() == Some("null"))
        );
    }

    #[test]
    fn recursive_receipt_ranges_separate_targets_stamps_and_each_state_limit() {
        let db = fixture();
        input(&db, "wanted", Some("null"), "accepted");
        db.execute_batch("UPDATE node SET created_at='same' WHERE id='wanted'; UPDATE receipt SET created_at='same' WHERE id='wanted';").unwrap();
        for (prefix, target, stamp, state, count) in [
            ("other-target", "parent", "same", "applied", 129),
            ("other-stamp", "child", "elsewhere", "applied", 129),
            ("valid-state", "child", "same", "held", 128),
        ] {
            for ordinal in 0..count {
                let id = format!("{prefix}-{ordinal}");
                db.execute(
                    "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user',?2,'{}')",
                    params![id, stamp],
                )
                .unwrap();
                db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES(?1,?1,'null',?2,?1,?3,?4)",params![id,target,state,stamp]).unwrap();
            }
        }
        let statements = crate::frontend::sql_trace::Statements::new(&db);
        let facts = read_tx(&db, Some("child"), &["wanted".into()]).unwrap();
        assert_eq!(statements.count(), 4);
        assert_eq!(
            facts.rows[0].sender.as_deref(),
            Some("null"),
            "129 across two states is valid; unrelated overflowing ranges cannot poison this source"
        );
        drop(statements);
        db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('overflow','user','same','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES('overflow','overflow','null','child','overflow','held','same');").unwrap();
        let statements = crate::frontend::sql_trace::Statements::new(&db);
        let facts = read_tx(&db, Some("child"), &["wanted".into()]).unwrap();
        assert_eq!(statements.count(), 3);
        assert!(
            facts.rows[0].sender.is_none(),
            "129th held witness invalidates the whole target/stamp bucket even when the requested accepted receipt matched"
        );
    }

    #[test]
    fn equal_stamp_limit_is_checked_before_request_filter_or_sender_copy() {
        let db = fixture();
        for index in 0..128 {
            input(&db, &format!("same-{index}"), Some("null"), "applied");
        }
        db.execute_batch(
            "UPDATE node SET created_at='same'; UPDATE receipt SET created_at='same';",
        )
        .unwrap();
        let ids = ["same-0".into(), "same-127".into()];
        let facts = read_tx(&db, Some("child"), &ids).unwrap();
        assert!(
            facts
                .rows
                .iter()
                .all(|row| row.sender.as_deref() == Some("null"))
        );
        input(&db, "extra", Some("null"), "applied");
        db.execute_batch("UPDATE node SET created_at='same' WHERE id='extra'; UPDATE receipt SET created_at='same' WHERE id='extra';").unwrap();
        let statements = crate::frontend::sql_trace::Statements::new(&db);
        let facts = read_tx(&db, Some("child"), &ids).unwrap();
        assert_eq!(
            statements.count(),
            3,
            "scope+common+one overflow receipt batch, no sender copy"
        );
        assert!(facts.rows.iter().all(|row| row.sender.is_none()));
        assert!(
            facts
                .rows
                .iter()
                .all(|row| project(row, Some("child"), Some("parent")).source
                    == InputSource::Unknown)
        );
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
            .query_map(
                params![
                    r#"[{"target":"child","stamp":"collision"}]"#,
                    "[\"collision-0\"]"
                ],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(plan.contains("receipt_target"), "{plan}");
        assert!(plan.contains("created_at=?"), "{plan}");
        assert!(RECEIPTS_AT_STAMP.contains("c.depth<129"));
        assert!(RECEIPTS_AT_STAMP.contains("AS MATERIALIZED"));
        assert!(!RECEIPTS_AT_STAMP.contains("sender_json"));
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
