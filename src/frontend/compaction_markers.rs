//! Render selected compaction facts already read by the owner.
use crate::{
    presentation::{PresentationKind, messages::Message},
    protocol::HistoryWindowCompaction,
    ui::UiNode,
};

pub(super) fn apply(nodes: &mut [UiNode], markers: &[HistoryWindowCompaction]) {
    for marker in markers {
        let Some(node) = nodes.iter_mut().find(|node| node.node_id == marker.node_id) else {
            continue;
        };
        if node.presentation.kind == PresentationKind::Summary {
            node.presentation.title = marker.facts.render(crate::config::UiLanguage::Ja);
            node.presentation.ui_messages.title =
                Some(Message::CompactionMarker(marker.facts.clone()));
        } else if !marker.facts.has_summary {
            node.presentation.compaction_note = Some(marker.facts.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UiLanguage;
    use crate::history::window_facts::compactions::facts;
    use rusqlite::Connection;
    use rusqlite::params;
    use serde_json::{Value, json};

    fn node(db: &Connection, id: &str, kind: &str, at: &str, payload: Value) {
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,?2,?3,?4)",
            params![id, kind, at, payload.to_string()],
        )
        .unwrap();
    }
    fn ui(id: &str, role: &str, payload: Value) -> UiNode {
        let text = crate::history::text_of_payload(&payload);
        UiNode {
            node_id: id.into(),
            role: role.into(),
            total_chars: text.chars().count(),
            presentation: crate::presentation::project(role, &payload, &text, 0),
            text,
            base_char_offset: 0,
            reasoning_ranges: vec![],
        }
    }
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        crate::readback::register(&db).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','2026-11-01T00:00:00Z'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','2026-11-01T00:00:00Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','2026-11-01T00:00:00Z','2026-11-01T01:00:00Z');").unwrap();
        node(
            &db,
            "original",
            "message",
            "2026-11-01T00:00:00Z",
            json!({}),
        );
        // Adopted automatic summary at 00:10 with its attempt, and a later skipped attempt.
        node(
            &db,
            "sum",
            "summary",
            "2026-11-01T00:10:00Z",
            json!({"item":{"role":"developer","content":[{"type":"input_text","text":"body"}]},"source_node_ids":["original"]}),
        );
        db.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES('sum','sum','sum','shellboy-summary-v1','v1','completed','2026-11-01T00:10:00Z')", []).unwrap();
        db.execute(
            "INSERT INTO thread_replacement(thread_id,replacement_id)VALUES('t','sum')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES('sum',0,'original',0)", []).unwrap();
        node(
            &db,
            "resp1",
            "assistant_response",
            "2026-11-01T00:09:00Z",
            json!({}),
        );
        node(
            &db,
            "resp2",
            "assistant_response",
            "2026-11-01T00:20:00Z",
            json!({}),
        );
        let attempt = |id: &str, at: &str, record: Value| {
            db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),?1,'u',0,0,'p','m',0,'completed','{}',?2,?3,?3)",
                params![id, json!({"input_tokens":32000}).to_string(), at]).unwrap();
            db.execute("INSERT INTO automatic_compaction(attempt_id,turn_id,attempt_started_at,reason,outcome,trigger_json,result_json) VALUES(?1,'u',?2,?3,?4,?5,?6)", params![id,at,record["reason"].as_str(),record["outcome"].as_str(),record.get("trigger").map(Value::to_string),json!({"metrics":record.get("summary")}).to_string()]).unwrap();
        };
        attempt(
            "a1",
            "2026-11-01T00:09:30Z",
            json!({"reason":"proactive_threshold","outcome":"adopted","trigger":{"auto_at_tokens":200000,"input_tokens":205000},"summary":{"summary_node_id":"sum","source_nodes":412,"summary_characters":31000,"model_requests":1}}),
        );
        attempt(
            "a2",
            "2026-11-01T00:19:30Z",
            json!({"reason":"proactive_threshold","outcome":"no_target","trigger":{"auto_at_tokens":200000,"input_tokens":206000}}),
        );
        for (n, a) in [("resp1", "a1"), ("resp2", "a2")] {
            db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES(?1,0,'message','completed','{}',?2)", params![a, n]).unwrap();
        }
        db
    }
    #[test]
    fn summary_row_and_skipped_attempt_note_use_saved_facts() {
        let db = fixture();
        let mut nodes = vec![
            ui(
                "sum",
                "summary",
                json!({"text":"body","source_node_ids":["original"]}),
            ),
            ui("resp1", "assistant_response", json!({"output":[]})),
            ui("resp2", "assistant_response", json!({"output":[]})),
        ];
        let observed = facts(
            &db,
            "t",
            &[
                ("sum".into(), "summary".into()),
                ("resp1".into(), "assistant_response".into()),
                ("resp2".into(), "assistant_response".into()),
            ],
        )
        .unwrap();
        apply(&mut nodes, &observed);
        let facts = match &nodes[0].presentation.ui_messages.title {
            Some(Message::CompactionMarker(facts)) => facts.clone(),
            other => panic!("{other:?}"),
        };
        assert!(facts.automatic && facts.has_summary);
        assert_eq!(facts.fired_at, Some(205000));
        assert_eq!(facts.source_nodes, Some(412));
        assert_eq!(facts.summary_chars, Some(31000));
        assert!(nodes[0].presentation.title.starts_with("自動要約："));
        // The first request after the adoption reported 32K input.
        assert!(
            facts.render(UiLanguage::En).ends_with("input after 32K"),
            "{}",
            facts.render(UiLanguage::En)
        );
        // Adopted-with-summary attempts do not duplicate onto the response.
        assert!(nodes[1].presentation.compaction_note.is_none());
        let note = nodes[2].presentation.compaction_note.clone().unwrap();
        assert_eq!(
            note.outcome,
            crate::presentation::compaction_marker::Outcome::Skipped
        );
        assert_eq!(note.fired_at, Some(206000));
    }
    #[test]
    fn a_refresh_that_changes_only_the_note_or_marker_changes_the_render_stamp() {
        let mut node = ui("resp2", "assistant_response", json!({"output":[]}));
        super::super::streams::seal_node(&mut node);
        let plain = node.presentation.projection_stamp;
        assert!(plain.is_some());
        let facts = crate::presentation::compaction_marker::MarkerFacts::from_event(
            &json!({"kind":"automatic_reduction","state":"no_target","metrics":{}}),
        )
        .unwrap();
        node.presentation.compaction_note = Some(facts.clone());
        super::super::streams::seal_node(&mut node);
        let noted = node.presentation.projection_stamp;
        assert_ne!(plain, noted);
        node.presentation.compaction_note = None;
        node.presentation.ui_messages.title = Some(Message::CompactionMarker(facts));
        super::super::streams::seal_node(&mut node);
        assert_ne!(plain, node.presentation.projection_stamp);
        assert_ne!(noted, node.presentation.projection_stamp);
    }
    #[test]
    fn pages_without_summaries_or_automatic_attempts_do_no_compaction_read() {
        let db = fixture();
        assert!(facts(&db, "t", &[]).unwrap().is_empty());
        assert!(
            facts(
                &db,
                "t",
                &[("original".into(), "assistant_response".into())]
            )
            .unwrap()
            .is_empty()
        );
    }
}
