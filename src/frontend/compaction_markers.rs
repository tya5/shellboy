//! Compaction markers for the conversation (#230): a summary row names how it
//! came about, and a skipped, failed or spill-only automatic attempt leaves one
//! note after the response that triggered it. Saved facts only, read on the DB
//! worker and projected on the presentation worker. A page without a summary or
//! an automatic attempt does no compaction read.
use crate::{
    control::Control,
    presentation::{PresentationKind, compaction_marker::MarkerFacts, messages::Message},
    protocol::{ApiError, CompactListArgs},
    storage::StorageError,
    ui::UiNode,
};
use rusqlite::Connection;

const MAX_NODES: usize = 128;
const AUTOMATIC_ATTEMPTS: &str = "SELECT oi.node_id,oi.attempt_id FROM output_item oi INDEXED BY output_item_node JOIN model_attempt a ON a.id=oi.attempt_id WHERE oi.node_id IN (SELECT value FROM json_each(?1)) AND CASE WHEN json_valid(a.error_json) THEN json_type(a.error_json,'$.automatic_trim_attempt')='object' ELSE 0 END";

pub(super) struct Raw {
    attempts: Vec<(String, String)>,
    events: crate::readback::RawCompactions,
}
pub(super) fn read_tx(
    db: &Connection,
    thread: &str,
    summaries: usize,
    assistants: &[String],
) -> Result<Option<Raw>, StorageError> {
    let encoded = serde_json::to_string(assistants).expect("String IDs serialize");
    let attempts = if assistants.is_empty() {
        Vec::new()
    } else {
        db.prepare_cached(AUTOMATIC_ATTEMPTS)?
            .query_map([encoded], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    if summaries == 0 && attempts.is_empty() {
        return Ok(None);
    }
    let args = CompactListArgs {
        thread_id: Some(thread.into()),
        since: None,
        until: None,
        limit: 200,
    };
    let events =
        crate::readback::read_compactions_tx(db, thread, &args).map_err(StorageError::from)?;
    Ok(Some(Raw { attempts, events }))
}

/// Applies prepared events to the page. Unmatched nodes keep their ordinary row.
pub(super) fn apply(nodes: &mut [UiNode], raw: Raw) -> Result<(), ApiError> {
    let page = crate::readback::prepare_compactions(raw.events)?;
    let items = page["items"].as_array().cloned().unwrap_or_default();
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
            node.presentation.projection_stamp = None;
        }
    }
    for (node_id, attempt) in raw.attempts {
        let Some(node) = nodes.iter_mut().find(|node| node.node_id == node_id) else {
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
            node.presentation.projection_stamp = None;
        }
    }
    Ok(())
}

pub(super) async fn enrich(
    control: &Control,
    viewed_thread: Option<&str>,
    nodes: &mut [UiNode],
) -> Result<(), ApiError> {
    let Some(thread) = viewed_thread.map(str::to_owned) else {
        return Ok(());
    };
    if nodes.len() > MAX_NODES {
        return Ok(());
    }
    let summaries = nodes
        .iter()
        .filter(|node| node.presentation.kind == PresentationKind::Summary)
        .count();
    let assistants = nodes
        .iter()
        .filter(|node| node.presentation.kind == PresentationKind::Assistant)
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    if summaries == 0 && assistants.is_empty() {
        return Ok(());
    }
    // Markers are cosmetic: a failed or unreadable compaction read leaves the
    // ordinary rows, and the page's own reads still report storage failures.
    let Ok(Some(raw)) = control
        .store
        .database
        .call(move |db| read_tx(db, &thread, summaries, &assistants))
        .await
    else {
        return Ok(());
    };
    let owned = nodes.iter_mut().map(std::mem::take).collect::<Vec<_>>();
    let projected = super::presentation_work(move || {
        let mut owned = owned;
        let result = apply(&mut owned, raw);
        (owned, result)
    })
    .await?;
    let (projected, _unreadable_events_keep_ordinary_rows) = projected;
    for (target, node) in nodes.iter_mut().zip(projected) {
        *target = node;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UiLanguage;
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
            db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,usage_json,started_at,ended_at)VALUES(?1,'u',0,0,'p','m',0,'completed','{}',?2,?3,?4,?4)",
                params![id, json!({"automatic_trim_attempt":record}).to_string(), json!({"input_tokens":32000}).to_string(), at]).unwrap();
        };
        attempt(
            "a1",
            "2026-11-01T00:09:30Z",
            json!({"reason":"proactive_threshold","outcome":"adopted","trigger":{"auto_at_tokens":200000,"input_tokens":205000},"summary":{"summary_node_id":"sum","source_nodes":412,"summary_characters":31000,"created_parts":1,"reused_parts":0}}),
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
        let raw = read_tx(&db, "t", 1, &["resp1".into(), "resp2".into()])
            .unwrap()
            .unwrap();
        apply(&mut nodes, raw).unwrap();
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
    fn pages_without_summaries_or_automatic_attempts_do_no_compaction_read() {
        let db = fixture();
        assert!(read_tx(&db, "t", 0, &[]).unwrap().is_none());
        assert!(
            read_tx(&db, "t", 0, &["original".into()])
                .unwrap()
                .is_none()
        );
    }
}
