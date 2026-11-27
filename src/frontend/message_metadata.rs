//! Bounded saved source facts for presentation, never inferred from rendered roles.
use crate::{
    control::Control,
    presentation::InputSource,
    protocol::{ApiError, ErrorCode},
    ui::UiNode,
};

pub(super) use crate::history::window_facts::message_sources::read_tx;
use crate::history::window_facts::message_sources::{MAX_NODES, RawSource, SourceFacts};
use crate::storage::StorageError;
#[cfg(test)]
use rusqlite::{Connection, params};

struct Metadata {
    created_at: Option<String>,
    source: InputSource,
    sender_label: Option<String>,
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
        node.presentation.input_sender_label = metadata.sender_label;
        use crate::presentation::ReplyState;
        node.presentation.reply_state = match raw.reply_state.as_deref() {
            Some("pending") => Some(ReplyState::Pending),
            Some("awaiting_own_ask") => Some(ReplyState::AwaitingOwnAsk),
            Some("replied") => Some(ReplyState::Replied),
            Some("ignored") => Some(ReplyState::Ignored),
            Some("cancelled") => Some(ReplyState::Cancelled),
            Some("recipient_deleted") => Some(ReplyState::RecipientDeleted),
            _ => None,
        };
        node.presentation.reply_excerpt = raw.reply_excerpt.as_deref().map(|v| {
            crate::presentation::sanitize_plain(v, 0)
                .0
                .replace(['\n', '\r'], " ")
                .chars()
                .take(512)
                .collect()
        });
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
    super::branch_markers::enrich(control, viewed_thread, nodes).await;
    super::compaction_markers::enrich(control, viewed_thread, nodes).await
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
        sender_label: crate::protocol::MessageSender::from_saved(raw.sender.as_deref())
            .label
            .filter(|label| !label.trim().is_empty()),
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
    use crate::history::window_facts::message_sources::{
        COMMON_SOURCES, RECEIPTS_AT_STAMP, STAMP_CANDIDATES,
    };
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
    fn reply_metadata_uses_saved_exact_node_and_refreshes_terminal_state() {
        let db = fixture();
        input(&db, "original", Some(r#"{"kind":"human"}"#), "applied");
        input(
            &db,
            "reply",
            Some(r#"{"kind":"model","thread_id":"parent"}"#),
            "applied",
        );
        db.execute("UPDATE node SET payload_json=json_object('content',json_array(json_object('type','input_text','text','source request'))) WHERE id='original'",[]).unwrap();
        db.execute("UPDATE receipt SET result_json=json_object('reply',json_object('state','pending')) WHERE id='original'",[]).unwrap();
        db.execute(
            "UPDATE receipt SET result_json=json_object('reply_to','original') WHERE id='reply'",
            [],
        )
        .unwrap();
        let mut nodes = vec![
            UiNode {
                node_id: "original".into(),
                ..Default::default()
            },
            UiNode {
                node_id: "reply".into(),
                ..Default::default()
            },
        ];
        let facts = read_tx(&db, Some("child"), &["original".into(), "reply".into()]).unwrap();
        apply(&mut nodes, &facts);
        assert_eq!(
            nodes[0].presentation.reply_state,
            Some(crate::presentation::ReplyState::Pending)
        );
        assert_eq!(
            nodes[1].presentation.reply_excerpt.as_deref(),
            Some("source request")
        );
        assert_eq!(nodes[1].presentation.input_source, InputSource::Parent);
        db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','replied') WHERE id='original'",[]).unwrap();
        let facts = read_tx(&db, Some("child"), &["original".into(), "reply".into()]).unwrap();
        apply(&mut nodes, &facts);
        assert_eq!(
            nodes[0].presentation.reply_state,
            Some(crate::presentation::ReplyState::Replied)
        );
    }
    #[test]
    fn common_tool_metadata_deduplicates_ids() {
        let db = fixture();
        let mut ids = Vec::new();
        for ordinal in 0..50 {
            let id = format!("tool-{ordinal}");
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','tool_result','saved-time','{}')", [&id]).unwrap();
            ids.push(id);
        }
        ids.extend(["tool-0".into(), "tool-49".into()]);
        let facts = read_tx(&db, Some("child"), &ids).unwrap();
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
            plan.contains("SEARCH n") && plan.contains("SEARCH u") && plan.contains("SEARCH r"),
            "{plan}"
        );
        assert!(
            !plan.contains("SCAN n") && !plan.contains("SCAN r"),
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
        db.execute_batch("UPDATE node SET created_at='shared'; UPDATE receipt SET created_at='shared'; INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES('second','second','null','child','ambiguous','held','shared'); INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('revisit','u','user','shared','{}'),('fake','u','event','shared','{\"kind\":\"revisit\"}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)VALUES('revisit','revisit','null','child','revisit','applied','delivered','{\"deliver\":\"complete\",\"delivery_context\":{\"reason\":\"run_completion\"}}');").unwrap();
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
        let facts = read_tx(&db, Some("child"), &ids).unwrap();
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
    fn distinct_user_timestamps_keep_sender_facts() {
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
        let facts = read_tx(&db, Some("child"), &ids).unwrap();
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
        let facts = read_tx(&db, Some("child"), &["wanted".into()]).unwrap();
        assert_eq!(
            facts.rows[0].sender.as_deref(),
            Some("null"),
            "129 across two states is valid; unrelated overflowing ranges cannot poison this source"
        );
        db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('overflow','user','same','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES('overflow','overflow','null','child','overflow','held','same');").unwrap();
        let facts = read_tx(&db, Some("child"), &["wanted".into()]).unwrap();
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
        let facts = read_tx(&db, Some("child"), &ids).unwrap();
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
    fn revisit_requires_saved_completion_delivery_and_collisions_are_bounded_unknown() {
        let db = fixture();
        db.execute_batch("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('revisit','u','user','time','{}'),('fake','u','event','time','{\"kind\":\"revisit\"}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)VALUES('revisit','revisit','null','child','revisit','applied','delivered','{\"deliver\":\"complete\",\"delivery_context\":{\"reason\":\"run_completion\"}}');").unwrap();
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
        let facts = read_tx(
            &db,
            Some("child"),
            &["named-cli".into(), "legacy-null".into()],
        )
        .unwrap();
        assert_eq!(facts.rows.len(), 2);
        assert_eq!(facts.rows[1].sender.as_deref(), Some("null"));
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
        assert_eq!(
            nodes[0].presentation.input_sender_label.as_deref(),
            Some("human")
        );
        assert!(nodes[1].presentation.input_sender_label.is_none());
        assert!(nodes.iter().all(|node|node.presentation.read_revision==77 && node.presentation.body_evicted));
    }

    #[test]
    fn source_sets_above_128_are_refused_without_silent_truncation() {
        let db = fixture();
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
