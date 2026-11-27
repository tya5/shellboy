//! Test-only application of owner window facts to saved nodes.
#[cfg(test)]
use crate::history::window_facts::read_window_facts;
#[cfg(test)]
use crate::storage::StorageError;

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
