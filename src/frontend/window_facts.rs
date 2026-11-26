//! Shared exact source correlation for native selected-window reads.
use crate::protocol::{HistoryWindowRecovery, HistoryWindowTools, HistoryWindowUsage};
use crate::storage::StorageError;
use rusqlite::Connection;

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
    let mut tools = super::tools::read_nodes_tx(db, &ids)?;
    let outside = extra
        .iter()
        .filter(|id| !ids.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    tools.append(super::tools::read_nodes_tx(db, &outside)?);
    let tools = tools.into_window();
    let selected = |include: fn(&str) -> bool| {
        items
            .iter()
            .enumerate()
            .filter(|(_, (_, kind))| include(kind))
            .map(|(index, (id, _))| (index, id.clone()))
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

/// Test path for saved nodes: the owner's window readers, then the client's
/// application of their facts, as one native install would apply them.
#[cfg(test)]
pub(crate) async fn apply_saved(
    control: &crate::control::Control,
    nodes: &mut [crate::ui::UiNode],
    language: crate::config::UiLanguage,
) -> Result<(), crate::protocol::ApiError> {
    use rusqlite::OptionalExtension;
    let ids = nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    let facts = control
        .store
        .database
        .call(move |db| {
            let mut items = Vec::with_capacity(ids.len());
            for id in ids {
                let kind: Option<String> = db
                    .query_row("SELECT kind FROM node WHERE id=?1", [&id], |row| row.get(0))
                    .optional()?;
                items.push((id, kind.unwrap_or_default()));
            }
            read_window_facts(db, &items, &[])
        })
        .await
        .map_err(StorageError::into_api_error)?;
    super::tools::apply_window(nodes, &facts.tools);
    super::recovery::apply_window(nodes, &facts.recovery, language);
    super::usage::apply_window(nodes, &facts.usage);
    Ok(())
}
