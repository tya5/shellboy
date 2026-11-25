//! Shared exact source correlation for native selected-window reads.
use crate::protocol::{
    HistorySnippet, HistoryWindowRecovery, HistoryWindowTools, HistoryWindowUsage,
};
use crate::storage::StorageError;
use rusqlite::Connection;

pub(crate) const WINDOW_FACTS_LIMIT: usize = 128;

pub(crate) struct WindowFacts {
    pub(crate) tools: HistoryWindowTools,
    pub(crate) recovery: Vec<HistoryWindowRecovery>,
    pub(crate) usage: Vec<HistoryWindowUsage>,
}

pub(crate) fn read_window_facts(
    db: &Connection,
    items: &[HistorySnippet],
) -> Result<WindowFacts, StorageError> {
    let ids = items
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    let tools = super::tools::read_nodes_tx(db, &ids)?.into_window();
    let selected = |include: fn(&str) -> bool| {
        items
            .iter()
            .enumerate()
            .filter(|(_, node)| include(&node.kind))
            .map(|(index, node)| (index, node.node_id.clone()))
            .collect::<Vec<_>>()
    };
    let recovery = super::recovery::read_tx(db, &selected(|kind| kind == "event"))?
        .into_iter()
        .map(|fact| fact.into_window(items))
        .collect();
    let usage = super::usage::read_tx(
        db,
        &selected(|kind| matches!(kind, "assistant_response" | "assistant")),
    )?
    .into_iter()
    .map(|fact| fact.into_window(items))
    .collect();
    Ok(WindowFacts {
        tools,
        recovery,
        usage,
    })
}
