//! Owner read of exact source correlation facts for selected-window observation.
pub(crate) mod compactions;
use crate::protocol::{HistoryWindowRecovery, HistoryWindowTools, HistoryWindowUsage};
use crate::storage::StorageError;
use rusqlite::Connection;

pub(crate) mod message_sources;
pub(crate) mod notices;
pub(crate) mod recovery;
pub(crate) mod tools;
pub(crate) mod usage;

pub(crate) const WINDOW_FACTS_LIMIT: usize = 128;

pub(crate) struct WindowFacts {
    pub(crate) tools: HistoryWindowTools,
    pub(crate) recovery: Vec<HistoryWindowRecovery>,
    pub(crate) usage: Vec<HistoryWindowUsage>,
}

/// Facts for the window's items, plus tool facts for `extra` saved nodes
/// outside it (each set at most 128), from the caller's transaction.
pub(crate) fn read_window_facts(
    db: &Connection,
    items: &[(String, String)],
    extra: &[String],
) -> Result<WindowFacts, StorageError> {
    let ids = items.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
    let mut tools = tools::read_nodes_tx(db, &ids)?;
    let outside = extra
        .iter()
        .filter(|id| !ids.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    tools.append(tools::read_nodes_tx(db, &outside)?);
    let tools = tools.into_window();
    let selected = |include: fn(&str) -> bool| {
        items
            .iter()
            .enumerate()
            .filter(|(_, (_, kind))| include(kind))
            .map(|(index, (id, _))| (index, id.clone()))
            .collect::<Vec<_>>()
    };
    let recovery = recovery::read_tx(db, &selected(|kind| kind == "event"))?
        .into_iter()
        .map(|fact| fact.into_window(items))
        .collect();
    let usage = usage::read_tx(
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
