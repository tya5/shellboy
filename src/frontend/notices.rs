//! Bounded current metadata for saved notifications; never reads raw output.
use crate::{
    presentation::{PresentationField, PresentationKind},
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
    ui::UiNode,
};
use rusqlite::Connection;

pub(super) const PREFIX: &str = "/ui_notice/";

// Named frontend copy keys for #35; never identity or lifecycle evidence.
enum NoticeText {
    SourceUnavailable,
    CallUnavailable,
    CommandUnrecorded,
    BackgroundRequested,
    BackgroundNotRequested,
    BackgroundUnknown,
    DeletedThread,
    ThreadUnavailable,
    Untitled,
}
impl NoticeText {
    fn text(self) -> &'static str {
        match self {
            Self::SourceUnavailable => "元の結果を参照できません",
            Self::CallUnavailable => "呼出し情報を参照できません",
            Self::CommandUnrecorded => "コマンド未記録",
            Self::BackgroundRequested => "背景指定あり",
            Self::BackgroundNotRequested => "背景指定なし",
            Self::BackgroundUnknown => "背景指定未記録",
            Self::DeletedThread => "参照先の会話は削除済み",
            Self::ThreadUnavailable => "参照先の会話を取得できません",
            Self::Untitled => "無題",
        }
    }
}

// Requested IDs stay outermost. Every relation is reached through an existing
// primary/unique key; a duplicate call ID in another group cannot supply facts.
const SQL: &str = r"
WITH requested AS (SELECT DISTINCT value AS node_id FROM json_each(?1)),
events AS MATERIALIZED (
 SELECT n.id AS node_id,e.kind,e.payload_ref_json AS payload
 FROM requested CROSS JOIN node n ON n.id=requested.node_id
 JOIN event e ON e.id=CASE WHEN json_valid(n.payload_json) THEN json_extract(n.payload_json,'$.event_id') END
 WHERE n.kind='event' AND e.kind IN ('shell_ended','child_turn_completed','child_run_ended','child_report')
), refs AS MATERIALIZED (
 SELECT events.*,CASE WHEN json_valid(payload) THEN json_extract(payload,'$.execution_id') END AS execution_id,
 CASE WHEN json_valid(payload) THEN json_extract(payload,'$.thread_id') END AS saved_thread,
 CASE WHEN json_valid(payload) THEN json_extract(payload,'$.run_id') END AS run_id FROM events
)
SELECT refs.node_id,refs.kind,refs.execution_id,
 substr(CASE WHEN json_valid(c.arguments_json) THEN CASE WHEN json_type(c.arguments_json,'$.command')='text' THEN json_extract(c.arguments_json,'$.command') END END,1,120),
 CASE WHEN json_valid(c.arguments_json) THEN CASE WHEN json_type(c.arguments_json,'$.background') IN ('true','false') THEN json_extract(c.arguments_json,'$.background') END END,
 c.group_id,c.call_id,r.node_id,
 COALESCE(refs.saved_thread,child.thread_id),substr(t.title,1,80),t.deleted_at,t.id,s.id,original.deleted_reason
FROM refs
LEFT JOIN shell_execution s ON s.id=refs.execution_id
LEFT JOIN tool_call c ON c.operation_id=s.id AND EXISTS(SELECT 1 FROM tool_group g WHERE g.id=c.group_id AND g.turn_id=s.turn_id)
LEFT JOIN tool_result r ON r.group_id=c.group_id AND r.call_id=c.call_id
LEFT JOIN node original ON original.id=r.node_id
LEFT JOIN run child ON child.id=refs.run_id
LEFT JOIN thread t ON t.id=COALESCE(refs.saved_thread,child.thread_id)
";

pub(super) struct Fact {
    pub node_id: String,
    kind: String,
    execution: Option<String>,
    command: Option<String>,
    background: Option<bool>,
    group: Option<String>,
    call: Option<String>,
    source: Option<String>,
    thread: Option<String>,
    title: Option<String>,
    deleted: Option<String>,
    available_thread: Option<String>,
    available_execution: Option<String>,
    source_deleted: Option<String>,
}

pub(super) fn read_tx(db: &Connection, ids: &[String]) -> Result<Vec<Fact>, StorageError> {
    if ids.len() > 128 || ids.iter().any(|id| id.is_empty() || id.len() > 256) {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Notification source scope exceeds128 nodes",
        )
        .into());
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let encoded = serde_json::to_string(ids).expect("notification IDs serialize");
    db.prepare(SQL)?
        .query_map([encoded], |r| {
            Ok(Fact {
                node_id: r.get(0)?,
                kind: r.get(1)?,
                execution: r.get(2)?,
                command: r.get(3)?,
                background: r.get(4)?,
                group: r.get(5)?,
                call: r.get(6)?,
                source: r.get(7)?,
                thread: r.get(8)?,
                title: r.get(9)?,
                deleted: r.get(10)?,
                available_thread: r.get(11)?,
                available_execution: r.get(12)?,
                source_deleted: r.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn clean(text: &str) -> String {
    crate::presentation::sanitize_plain(text, 0)
        .0
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn field(key: &str, value: String) -> PresentationField {
    PresentationField {
        key: format!("{PREFIX}{key}"),
        label: key.into(),
        value,
        ..Default::default()
    }
}

pub(super) fn apply(nodes: &mut [UiNode], facts: &[Fact]) {
    for node in nodes
        .iter_mut()
        .filter(|n| n.presentation.kind == PresentationKind::Event)
    {
        node.presentation
            .fields
            .retain(|f| !f.key.starts_with(PREFIX));
        let Some(fact) = facts.iter().find(|f| f.node_id == node.node_id) else {
            continue;
        };
        let mut fields = Vec::new();
        let caption = if fact.kind == "shell_ended" {
            if let Some(execution) = &fact.execution {
                fields.push(field("execution_id", execution.clone()));
            }
            if fact.source.is_none() || fact.source_deleted.is_some() {
                fields.push(field(
                    "source_error",
                    NoticeText::SourceUnavailable.text().into(),
                ));
            }
            if let (Some(group), Some(call), Some(source), None) =
                (&fact.group, &fact.call, &fact.source, &fact.source_deleted)
            {
                fields.push(field("group_id", group.clone()));
                fields.push(field("call_id", call.clone()));
                fields.push(field("source_node", source.clone()));
            }
            if fact.available_execution.is_none() || fact.group.is_none() {
                NoticeText::CallUnavailable.text().into()
            } else {
                let command = fact
                    .command
                    .as_deref()
                    .map(clean)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| NoticeText::CommandUnrecorded.text().into());
                let background = match fact.background {
                    Some(true) => NoticeText::BackgroundRequested.text(),
                    Some(false) => NoticeText::BackgroundNotRequested.text(),
                    None => NoticeText::BackgroundUnknown.text(),
                };
                format!("{command} · {background}")
            }
        } else {
            if let Some(thread) = &fact.thread {
                fields.push(field("thread_id", thread.clone()));
            }
            if fact.deleted.is_some() {
                NoticeText::DeletedThread.text().into()
            } else if fact.available_thread.is_none() {
                NoticeText::ThreadUnavailable.text().into()
            } else {
                format!(
                    "現在の名前: {}",
                    clean(
                        fact.title
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or(NoticeText::Untitled.text())
                    )
                )
            }
        };
        if fact.kind != "shell_ended" && (fact.deleted.is_some() || fact.available_thread.is_none())
        {
            fields.push(field("unavailable", caption.clone()));
        }
        fields.push(field(
            "title",
            format!("{} · {caption}", node.presentation.title),
        ));
        let remaining = crate::presentation::MAX_FIELDS.saturating_sub(fields.len());
        if node.presentation.fields.len() > remaining {
            node.presentation.fields.truncate(remaining);
            node.presentation.metadata_limited = true;
        }
        node.presentation.fields.extend(fields);
        node.presentation.bound_metadata();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','same','/','','p','m','now'),('child','same','/','','p','m','now');
          INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent','completed','now'),('child-run','child','completed','now');
          INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','parent','parent-run','completed','now'),('v','child','child-run','completed','now');
          INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','now'),('b','v',0,0,'p','m',0,'completed','{}','now');
          INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','u','completed','now'),('other','b','v','completed','now');
          INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('g','same',0,'shell','{\"command\":\"printf CORRECT\",\"background\":true}','completed','exec'),('other','same',0,'shell','{\"command\":\"printf WRONG\"}','completed','other-exec');
          INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('exec','parent-run','u','exec','exited','/','{}','UTF-8','UTF-8','closed','now'),('other-exec','child-run','v','other-exec','exited','/','{}','UTF-8','UTF-8','closed','now');
          INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('source','u','tool_result','now','{\"text\":\"RAW_OUTPUT_MUST_NOT_BE_FETCHED\"}'),('wrong','v','tool_result','now','{}');
          INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','same','source','exited'),('other','same','wrong','exited');").unwrap();
        for (id, kind, payload) in [
            ("shell", "shell_ended", json!({"execution_id":"exec"})),
            (
                "child-notice",
                "child_run_ended",
                json!({"run_id":"child-run"}),
            ),
            (
                "missing",
                "child_report",
                json!({"thread_id":"missing-thread"}),
            ),
        ] {
            db.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES(?1,?2,'{}','parent',?3,'now')",rusqlite::params![id,kind,payload.to_string()]).unwrap();
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','event','now',?2)",rusqlite::params![format!("node-{id}"),json!({"event_id":id}).to_string()]).unwrap();
        }
        db
    }
    fn node(id: &str) -> UiNode {
        UiNode {
            node_id: format!("node-{id}"),
            text: "CANONICAL_SENTINEL".into(),
            presentation: crate::presentation::NodePresentation {
                kind: PresentationKind::Event,
                title: "saved notice".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    fn value<'a>(node: &'a UiNode, key: &str) -> Option<&'a str> {
        node.presentation
            .fields
            .iter()
            .find(|f| f.key == format!("{PREFIX}{key}"))
            .map(|f| f.value.as_str())
    }
    #[test]
    fn exact_execution_and_run_sources_use_one_bounded_metadata_statement() {
        let db = fixture();
        let ids = vec![
            "node-shell".into(),
            "node-child-notice".into(),
            "node-missing".into(),
            "node-shell".into(),
        ];
        let trace = super::super::sql_trace::Statements::new(&db);
        let facts = read_tx(&db, &ids).unwrap();
        assert_eq!(trace.count(), 1);
        assert_eq!(facts.len(), 3);
        let mut nodes = vec![node("shell"), node("child-notice"), node("missing")];
        apply(&mut nodes, &facts);
        assert_eq!(value(&nodes[0], "source_node"), Some("source"));
        assert_eq!(value(&nodes[0], "group_id"), Some("g"));
        assert!(value(&nodes[0], "title").unwrap().contains("CORRECT"));
        assert!(!value(&nodes[0], "title").unwrap().contains("WRONG"));
        assert!(value(&nodes[0], "title").unwrap().contains("背景指定あり"));
        assert_eq!(value(&nodes[1], "thread_id"), Some("child"));
        assert!(
            value(&nodes[1], "title")
                .unwrap()
                .contains("現在の名前: same")
        );
        assert!(value(&nodes[2], "unavailable").is_some());
        for node in &nodes {
            assert_eq!(node.text, "CANONICAL_SENTINEL");
            assert!(node.presentation.metadata_bytes() <= crate::presentation::MAX_METADATA_BYTES);
            assert!(
                !format!("{:?}", node.presentation.fields)
                    .contains("RAW_OUTPUT_MUST_NOT_BE_FETCHED")
            );
        }
        drop(trace);
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {SQL}"))
            .unwrap()
            .query_map([serde_json::to_string(&ids).unwrap()], |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(plan.contains("SEARCH n USING INDEX"), "{plan}");
        assert!(plan.contains("SEARCH c USING INDEX"), "{plan}");
        assert!(
            !plan.contains("SCAN shell_execution") && !plan.contains("SCAN tool_call"),
            "{plan}"
        );
    }
    #[test]
    fn rename_delete_and_missing_refs_replace_current_metadata_without_touching_history() {
        let db = fixture();
        let ids = vec!["node-child-notice".into()];
        let mut nodes = vec![node("child-notice")];
        apply(&mut nodes, &read_tx(&db, &ids).unwrap());
        db.execute("UPDATE thread SET title='renamed' WHERE id='child'", [])
            .unwrap();
        apply(&mut nodes, &read_tx(&db, &ids).unwrap());
        assert!(
            value(&nodes[0], "title")
                .unwrap()
                .contains("現在の名前: renamed")
        );
        assert!(!value(&nodes[0], "title").unwrap().contains("same"));
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='child'", [])
            .unwrap();
        apply(&mut nodes, &read_tx(&db, &ids).unwrap());
        assert!(
            value(&nodes[0], "unavailable")
                .unwrap()
                .contains("削除済み")
        );
        assert_eq!(value(&nodes[0], "thread_id"), Some("child"));
        assert_eq!(nodes[0].text, "CANONICAL_SENTINEL");
        let trace = super::super::sql_trace::Statements::new(&db);
        assert!(read_tx(&db, &vec!["x".into(); 129]).is_err());
        assert!(read_tx(&db, &[]).unwrap().is_empty());
        assert_eq!(trace.count(), 0);
    }
}
