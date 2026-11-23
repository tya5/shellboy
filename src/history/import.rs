//! Materialize a validated export under the ordinary writer transaction.
use super::sql_error;
use crate::protocol::{ApiError, HistoryExportReferenceScope::Internal};
use crate::thread_import::{Import, parts};
use rusqlite::{Transaction, params};
use std::collections::HashMap;

pub(crate) fn write(tx: &Transaction<'_>, thread: &str, import: Import) -> Result<(), ApiError> {
    let ids: HashMap<_, _> = import
        .records
        .iter()
        .map(|record| {
            (
                parts(record).1.id.as_str(),
                uuid::Uuid::new_v4().to_string(),
            )
        })
        .collect();
    let mut hashes = HashMap::new();
    for record in &import.records {
        let (_, node, graph, reason) = parts(record);
        let body = node.payload_json.as_deref();
        if let Some(body) = body {
            let payload: serde_json::Value = serde_json::from_str(body)
                .map_err(|_| crate::thread_import::invalid("Invalid imported node payload"))?;
            hashes.insert(node.id.as_str(), crate::compaction::key(&(0_i64, &payload)));
        }
        let mut origins = node.import_origins.clone();
        origins.push(crate::protocol::ImportOrigin {
            source_thread_id: import.source_thread.clone(),
            source_node_id: node.id.clone(),
            source_provenance: node.provenance.clone(),
            source_graph: graph.clone(),
        });
        let origin = serde_json::to_string(&origins)
            .map_err(|_| crate::thread_import::invalid("Cannot serialize import origins"))?;
        tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash,deleted_reason,import_origin_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![ids[node.id.as_str()],node.provenance.kind,node.provenance.created_at,body,body.map_or(0,str::len) as i64,body.map(crate::digest::sha256_hex),reason,origin]).map_err(sql_error)?;
    }
    let mut tip = None;
    for record in &import.records {
        let (_, node, graph, _) = parts(record);
        let id = &ids[node.id.as_str()];
        if let Some(ordinal) = node.current_ordinal {
            tx.execute(
                "INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES(?1,?2,?3)",
                params![thread, ordinal, id],
            )
            .map_err(sql_error)?;
            if tip.is_none_or(|(last, _)| ordinal > last) {
                tip = Some((ordinal, id));
            }
        }
        for parent in graph
            .previous
            .iter()
            .filter(|reference| reference.scope == Internal)
        {
            tx.execute(
                "INSERT INTO edge(src,dst) VALUES(?1,?2)",
                params![ids[parent.id.as_str()], id],
            )
            .map_err(sql_error)?;
        }
        for group in &graph.replacement_groups {
            let group_id = uuid::Uuid::new_v4().to_string();
            // Import lineage must never become a candidate for summary reuse.
            tx.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES(?1,?2,?1,'import','import',?3,?4)", params![group_id,id,group.state,node.provenance.created_at]).map_err(sql_error)?;
            tx.execute(
                "INSERT INTO thread_replacement(thread_id,replacement_id) VALUES(?1,?2)",
                params![thread, group_id],
            )
            .map_err(sql_error)?;
            for source in group
                .sources
                .iter()
                .filter(|source| source.node.scope == Internal)
            {
                tx.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision,source_hash) VALUES(?1,?2,?3,0,?4)", params![group_id,source.ordinal,ids[source.node.id.as_str()],hashes.get(source.node.id.as_str())]).map_err(sql_error)?;
            }
        }
    }
    tx.execute(
        "UPDATE thread SET cursor_node_id=?2,selection_revision=1,revision=1 WHERE id=?1",
        params![thread, tip.map(|(_, id)| id)],
    )
    .map_err(sql_error)?;
    Ok(())
}
