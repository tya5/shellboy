//! Compaction markers for the conversation (#230): a summary row names how it
//! came about, and a skipped, failed or spill-only automatic attempt leaves one
//! note after the response that triggered it. Saved facts only: the owner's
//! `compact.list` joined to each response's provenance `attempt_id`. A page
//! without a summary or an attempt-bound response reads nothing.
use super::native_history::Owner;
use crate::{
    presentation::{PresentationKind, compaction_marker::MarkerFacts, messages::Message},
    protocol::{ApiError, Command, CompactListArgs, CompactListResult},
    ui::UiNode,
};
use serde_json::Value;

/// The Thread's compaction events, when the page has a node they can mark.
pub(super) async fn read(
    owner: &Owner<'_>,
    thread: &str,
    nodes: &[UiNode],
    attempts: &[(String, String)],
) -> Result<Vec<Value>, ApiError> {
    let summaries = nodes
        .iter()
        .any(|node| node.presentation.kind == PresentationKind::Summary);
    if !summaries && attempts.is_empty() {
        return Ok(Vec::new());
    }
    let page: CompactListResult = owner
        .call(Command::CompactList(CompactListArgs {
            thread_id: Some(thread.into()),
            since: None,
            until: None,
            limit: 200,
        }))
        .await?;
    Ok(page.items)
}

/// Applies the events to the page; `attempts` pairs each response node with
/// its saved attempt. Unmatched nodes keep their ordinary row.
pub(super) fn apply(nodes: &mut [UiNode], items: &[Value], attempts: &[(String, String)]) {
    let by_summary = |id: &str| {
        items
            .iter()
            .find(|item| item["metrics"]["summary_node_id"].as_str() == Some(id))
    };
    for node in nodes.iter_mut() {
        if node.presentation.kind == PresentationKind::Summary
            && let Some(facts) = by_summary(&node.node_id).and_then(MarkerFacts::from_event)
        {
            node.presentation.title = facts.render(crate::config::UiLanguage::Ja);
            node.presentation.ui_messages.title = Some(Message::CompactionMarker(facts));
        }
    }
    for (node_id, attempt) in attempts {
        let Some(node) = nodes.iter_mut().find(|node| &node.node_id == node_id) else {
            continue;
        };
        let event = items.iter().find(|item| {
            item["attempt_id"].as_str() == Some(attempt.as_str())
                && item["reason"] == "proactive_threshold"
        });
        // A summary is drawn on its own row; this note covers everything else.
        if let Some(facts) = event
            .and_then(MarkerFacts::from_event)
            .filter(|facts| !facts.has_summary)
        {
            node.presentation.compaction_note = Some(facts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UiLanguage;
    use rusqlite::{Connection, params};
    use serde_json::json;

    /// The owner's `compact.list` items for the fixture Thread.
    fn items(db: &Connection) -> Vec<Value> {
        let args = CompactListArgs {
            thread_id: Some("t".into()),
            since: None,
            until: None,
            limit: 200,
        };
        crate::readback::compactions(db, "t", &args).unwrap()["items"]
            .as_array()
            .cloned()
            .unwrap()
    }

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
        let attempts = [("resp1".into(), "a1".into()), ("resp2".into(), "a2".into())];
        apply(&mut nodes, &items(&db), &attempts);
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
    fn a_response_without_an_automatic_attempt_keeps_its_ordinary_row() {
        let db = fixture();
        let mut nodes = vec![ui("resp1", "assistant_response", json!({"output":[]}))];
        apply(&mut nodes, &items(&db), &[("resp1".into(), "other".into())]);
        assert!(nodes[0].presentation.compaction_note.is_none());
    }
}
