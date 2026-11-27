//! Bounded compaction facts attached to exact selected history nodes.
use crate::{
    presentation::compaction_marker::MarkerFacts,
    protocol::{CompactListArgs, HistoryWindowCompaction},
    storage::StorageError,
};
use rusqlite::Connection;

const AUTOMATIC_ATTEMPTS: &str = "SELECT oi.node_id,oi.attempt_id FROM output_item oi INDEXED BY output_item_node JOIN automatic_compaction c ON c.attempt_id=oi.attempt_id WHERE oi.node_id IN (SELECT value FROM json_each(?1))";

struct Raw {
    attempts: Vec<(String, String)>,
    events: crate::readback::RawCompactions,
}
fn read_tx(
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

pub(crate) fn facts(
    db: &Connection,
    thread: &str,
    items: &[(String, String)],
) -> Result<Vec<HistoryWindowCompaction>, StorageError> {
    let summaries = items.iter().filter(|(_, kind)| kind == "summary").count();
    let assistants = items
        .iter()
        .filter(|(_, kind)| matches!(kind.as_str(), "assistant" | "assistant_response"))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let Some(raw) = read_tx(db, thread, summaries, &assistants)? else {
        return Ok(Vec::new());
    };
    let page = crate::readback::prepare_compactions(raw.events).map_err(StorageError::from)?;
    let Some(events) = page["items"].as_array() else {
        return Ok(Vec::new());
    };
    let mut markers = Vec::new();
    for (node_id, kind) in items {
        let summary = kind == "summary";
        let event = if summary {
            events.iter().find(|event| {
                event["metrics"]["summary_node_id"].as_str() == Some(node_id.as_str())
            })
        } else {
            raw.attempts
                .iter()
                .find(|(id, _)| id == node_id)
                .and_then(|(_, attempt)| {
                    events.iter().find(|event| {
                        event["attempt_id"].as_str() == Some(attempt.as_str())
                            && event["reason"] == "proactive_threshold"
                    })
                })
        };
        if let Some(facts) = event
            .and_then(MarkerFacts::from_event)
            .filter(|facts| summary || !facts.has_summary)
        {
            markers.push(HistoryWindowCompaction {
                node_id: node_id.clone(),
                facts,
            });
        }
    }
    Ok(markers)
}
