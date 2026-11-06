//! Branch markers for the conversation (#237): a node where the viewed Thread's
//! branches diverge gets one row. Saved facts only, read on the DB worker with
//! the page; a Thread with no parent and no child does one indexed existence
//! read. Cosmetic: a failed read leaves the ordinary rows.
use crate::{control::Control, presentation::branch_marker::BranchMarker, ui::UiNode};

const MAX_NODES: usize = 128;

pub(super) async fn enrich(control: &Control, viewed_thread: Option<&str>, nodes: &mut [UiNode]) {
    let Some(thread) = viewed_thread.map(str::to_owned) else {
        return;
    };
    if nodes.is_empty() || nodes.len() > MAX_NODES {
        return;
    }
    let ids = nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    let Ok(Ok(markers)) = control
        .store
        .database
        .call(move |db| Ok(crate::history::branches::markers(db, &thread, &ids)))
        .await
    else {
        return;
    };
    for marker in markers {
        if let Some(node) = nodes
            .iter_mut()
            .find(|n| n.node_id == marker.anchor_node_id)
        {
            node.presentation.branch_marker = Some(BranchMarker::from_facts(&marker));
        }
    }
}
