//! Bounded current metadata for saved notifications; never reads raw output.
use crate::{
    presentation::{
        PresentationField, PresentationKind,
        messages::{Message, Text},
    },
    ui::UiNode,
};
#[cfg(test)]
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
        }
    }
}

pub(super) use crate::history::window_facts::notices::Fact;
#[cfg(test)]
use crate::history::window_facts::notices::*;

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
        node.presentation.ui_messages.notice = None;
        node.presentation.ui_messages.source_error = None;
        let Some(fact) = facts.iter().find(|f| f.node_id == node.node_id) else {
            continue;
        };
        let mut fields = Vec::new();
        let caption = {
            if let Some(description) = crate::tool_description::label(fact.description.as_deref()) {
                fields.push(field("description", description));
            }
            if let Some(execution) = &fact.execution_id {
                fields.push(field("execution_id", execution.clone()));
            }
            if fact.source_node_id.is_none() {
                fields.push(field(
                    "source_error",
                    NoticeText::SourceUnavailable.text().into(),
                ));
            }
            if let (Some(group), Some(call), Some(source)) =
                (&fact.group_id, &fact.call_id, &fact.source_node_id)
            {
                fields.push(field("group_id", group.clone()));
                fields.push(field("call_id", call.clone()));
                fields.push(field("source_node", source.clone()));
            }
            if !fact.execution_available || fact.group_id.is_none() {
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
                crate::ui::text_width::glyphs::formatted!(
                    "{command} · {background}",
                    "{command} | {background}"
                )
            }
        };
        fields.push(field(
            "title",
            crate::ui::text_width::glyphs::formatted!(
                "{} · {caption}",
                "{} | {caption}",
                node.presentation.title
            ),
        ));
        let remaining = crate::presentation::MAX_FIELDS.saturating_sub(fields.len());
        if node.presentation.fields.len() > remaining {
            node.presentation.fields.truncate(remaining);
            node.presentation.metadata_limited = true;
        }
        node.presentation.fields.extend(fields);
        let source_error = fact
            .source_node_id
            .is_none()
            .then_some(Text::NoticeSourceUnavailable);
        let notice = if !fact.execution_available || fact.group_id.is_none() {
            Message::Text(Text::NoticeCallUnavailable)
        } else {
            Message::CommandNotice {
                command: fact.command.as_deref().map(clean).filter(|s| !s.is_empty()),
                background: fact.background,
            }
        };
        node.presentation
            .ui_messages
            .set_notice(notice, source_error);
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
          INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'a','u',0,0,'p','m',0,'completed','{}','now'),('conversation',(SELECT thread_id FROM turn WHERE id='v'),'b','v',0,0,'p','m',0,'completed','{}','now');
          INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','u','completed','now'),('other','b','v','completed','now');
          INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('g','same',0,'shell','{\"command\":\"printf CORRECT\",\"background\":true}','completed','exec'),('other','same',0,'shell','{\"command\":\"printf WRONG\"}','completed','other-exec');
          INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('exec','parent-run','u','exec','exited','/','{}','UTF-8','UTF-8','closed','now'),('other-exec','child-run','v','other-exec','exited','/','{}','UTF-8','UTF-8','closed','now');
          INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('source','u','tool_result','now','{\"text\":\"RAW_OUTPUT_MUST_NOT_BE_FETCHED\"}'),('wrong','v','tool_result','now','{}');
          INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','same','source','exited'),('other','same','wrong','exited');").unwrap();
        let (id, kind, payload) = ("shell", "shell_ended", json!({"execution_id":"exec"}));
        db.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES(?1,?2,'{}','parent',?3,'now')",rusqlite::params![id,kind,payload.to_string()]).unwrap();
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','event','now',?2)",rusqlite::params![format!("node-{id}"),json!({"event_id":id}).to_string()]).unwrap();
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
    fn typed_current_notices_preserve_serialized_fields_and_clear_stale_facts() {
        let db = fixture();
        let ids = vec![
            "node-shell".into(),
            "node-child-notice".into(),
            "node-missing".into(),
        ];
        let mut facts = read_tx(&db, &ids).unwrap();
        let mut nodes = vec![node("shell"), node("child-notice"), node("missing")];
        apply(&mut nodes, &facts);
        assert_eq!(
            nodes[0].presentation.ui_messages.notice,
            Some(Message::CommandNotice {
                command: Some("printf CORRECT".into()),
                background: Some(true),
            })
        );
        assert_eq!(nodes[1].presentation.ui_messages.notice, None);
        assert_eq!(nodes[2].presentation.ui_messages.notice, None);
        let serialized = serde_json::to_value(&nodes[0].presentation).unwrap();
        let mut without_typed = nodes[0].presentation.clone();
        without_typed.ui_messages.notice = None;
        without_typed.ui_messages.source_error = None;
        assert_eq!(serialized, serde_json::to_value(&without_typed).unwrap());

        let shell = facts
            .iter_mut()
            .find(|fact| fact.node_id == "node-shell")
            .unwrap();
        shell.source_node_id = None;
        shell.group_id = None;
        apply(&mut nodes, &facts);
        assert_eq!(
            nodes[0].presentation.ui_messages.notice,
            Some(Message::Text(Text::NoticeCallUnavailable))
        );
        assert_eq!(
            nodes[0].presentation.ui_messages.source_error,
            Some(Text::NoticeSourceUnavailable)
        );
        assert!(value(&nodes[0], "source_error").is_some());
        assert_eq!(nodes[1].presentation.ui_messages.notice, None);
        apply(&mut nodes, &[]);
        for node in &nodes {
            assert_eq!(node.presentation.ui_messages.notice, None);
            assert_eq!(node.presentation.ui_messages.source_error, None);
            assert_eq!(value(node, "title"), None);
        }
    }

    #[test]
    fn notice_description_is_bounded_sanitized_and_does_not_replace_original_command() {
        let db = fixture();
        let raw = format!("{}inspect\n{}  ", " ".repeat(90), "x".repeat(100));
        let args = json!({"command": "printf CORRECT", "description": raw});
        db.execute(
            "UPDATE tool_call SET arguments_json=?1 WHERE group_id='g' AND call_id='same'",
            [args.to_string()],
        )
        .unwrap();
        let facts = read_tx(&db, &["node-shell".into()]).unwrap();
        let mut nodes = vec![node("shell")];
        apply(&mut nodes, &facts);
        let description = value(&nodes[0], "description").unwrap();
        assert!(description.starts_with("inspect "));
        assert!(description.chars().count() <= 80);
        assert!(!description.contains('\n'));
        assert_eq!(nodes[0].text, "CANONICAL_SENTINEL");
        assert!(
            value(&nodes[0], "title")
                .unwrap()
                .contains("printf CORRECT")
        );
    }

    #[test]
    fn exact_execution_and_run_sources_read_bounded_metadata() {
        let db = fixture();
        let ids = vec![
            "node-shell".into(),
            "node-child-notice".into(),
            "node-missing".into(),
            "node-shell".into(),
        ];
        let facts = read_tx(&db, &ids).unwrap();
        assert_eq!(facts.len(), 1);
        let mut nodes = vec![node("shell"), node("child-notice"), node("missing")];
        apply(&mut nodes, &facts);
        assert_eq!(value(&nodes[0], "source_node"), Some("source"));
        assert_eq!(value(&nodes[0], "group_id"), Some("g"));
        assert!(value(&nodes[0], "title").unwrap().contains("CORRECT"));
        assert!(!value(&nodes[0], "title").unwrap().contains("WRONG"));
        assert!(value(&nodes[0], "title").unwrap().contains("背景指定あり"));
        assert!(value(&nodes[1], "thread_id").is_none());
        assert!(value(&nodes[2], "unavailable").is_none());
        for node in &nodes {
            assert_eq!(node.text, "CANONICAL_SENTINEL");
            assert!(node.presentation.metadata_bytes() <= crate::presentation::MAX_METADATA_BYTES);
            assert!(
                !format!("{:?}", node.presentation.fields)
                    .contains("RAW_OUTPUT_MUST_NOT_BE_FETCHED")
            );
        }
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
    fn child_lifecycle_facts_are_not_decorated_or_refreshed() {
        let db = fixture();
        let ids = vec!["node-child-notice".into()];
        let mut nodes = vec![node("child-notice")];
        assert!(read_tx(&db, &ids).unwrap().is_empty());
        apply(&mut nodes, &[]);
        assert!(value(&nodes[0], "title").is_none());
        assert_eq!(nodes[0].text, "CANONICAL_SENTINEL");
        assert!(read_tx(&db, &vec!["x".into(); 129]).is_err());
        assert!(read_tx(&db, &[]).unwrap().is_empty());
    }
}
