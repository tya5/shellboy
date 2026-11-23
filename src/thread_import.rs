//! Validation for the single-request Thread import format.
use crate::protocol::{
    ApiError, ErrorCode, HistoryExportNode, HistoryExportRecord, HistoryExportReference,
    HistoryExportReferenceScope, HistoryExportRelations, HistoryExportRole, MAX_FRAME_BYTES,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::Path;

const MAX_NODES: usize = 1024;
const MAX_LINKS: usize = 4096;

#[derive(Deserialize)]
struct Header {
    #[serde(rename = "type")]
    kind: String,
    format: String,
    version: u32,
    core_protocol: u32,
    thread: SourceThread,
}

#[derive(Deserialize)]
struct SourceThread {
    thread_id: String,
}

#[derive(Deserialize)]
struct Trailer {
    #[serde(rename = "type")]
    kind: String,
    lines: usize,
    sha256: String,
}

pub(crate) struct Import {
    pub(crate) source_thread: String,
    pub(crate) records: Vec<HistoryExportRecord>,
}

pub(crate) fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}

pub(crate) fn too_large() -> ApiError {
    ApiError::new(
        ErrorCode::InputLimit,
        format!("Thread import must fit in one {MAX_FRAME_BYTES}-byte request frame"),
    )
}

pub fn read_file(path: &Path) -> Result<String, ApiError> {
    let io_error = |error: std::io::Error| {
        ApiError::new(
            ErrorCode::IoFailed,
            format!("Cannot read Thread import: {error}"),
        )
    };
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(io_error)?
        .take(MAX_FRAME_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(too_large());
    }
    let document = String::from_utf8(bytes).map_err(|_| invalid("Thread import is not UTF-8"))?;
    validate(&document)?;
    Ok(document)
}

pub(crate) fn validate(document: &str) -> Result<Import, ApiError> {
    if document.len() > MAX_FRAME_BYTES {
        return Err(too_large());
    }
    let body = document
        .strip_suffix('\n')
        .ok_or_else(|| invalid("Thread import requires a final LF"))?;
    let (prefix, trailer_text) = body
        .rsplit_once('\n')
        .ok_or_else(|| invalid("Thread import requires a header and trailer"))?;
    let trailer: Trailer =
        serde_json::from_str(trailer_text).map_err(|_| invalid("Invalid Thread import trailer"))?;
    let preceding = &document.as_bytes()[..prefix.len() + 1];
    let actual = Sha256::digest(preceding)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if trailer.kind != "trailer"
        || trailer.sha256 != actual
        || trailer.lines != preceding.iter().filter(|&&byte| byte == b'\n').count()
    {
        return Err(invalid(
            "Thread import trailer count or SHA-256 does not match",
        ));
    }
    if trailer.lines > MAX_NODES + 1 {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            format!("Thread import supports at most {MAX_NODES} nodes"),
        ));
    }
    let mut lines = prefix.split('\n');
    let header: Header = serde_json::from_str(lines.next().unwrap_or_default())
        .map_err(|_| invalid("Invalid Thread import header"))?;
    if header.kind != "header"
        || header.format != "shellboy-thread-export"
        || header.version != 1
        || header.core_protocol == 0
        || header.thread.thread_id.is_empty()
    {
        return Err(invalid("Unsupported Thread export format or version"));
    }
    let records = lines
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|_| {
                invalid("Invalid Thread export record").details(serde_json::json!({"line":index+2}))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_graph(&records)?;
    Ok(Import {
        source_thread: header.thread.thread_id,
        records,
    })
}

pub(crate) fn parts(
    record: &HistoryExportRecord,
) -> (
    &HistoryExportRole,
    &HistoryExportNode,
    &HistoryExportRelations,
    Option<&str>,
) {
    match record {
        HistoryExportRecord::Node {
            role,
            node,
            relations,
            ..
        } => (role, node, relations, None),
        HistoryExportRecord::Tombstone {
            role,
            node,
            relations,
            reason,
            ..
        } => (role, node, relations, Some(reason)),
    }
}

fn validate_graph(records: &[HistoryExportRecord]) -> Result<(), ApiError> {
    let malformed = || invalid("Thread export contains inconsistent node or graph metadata");
    let mut graph = HashMap::new();
    let mut selected = BTreeSet::new();
    let mut previous_order = 0;
    let mut links = 0usize;
    for record in records {
        let (role, node, relations, tombstone) = parts(record);
        if node.id.is_empty()
            || node.order <= previous_order
            || graph.insert(node.id.as_str(), relations).is_some()
        {
            return Err(malformed());
        }
        previous_order = node.order;
        match (role, node.current_ordinal) {
            (HistoryExportRole::Current, Some(ordinal)) if selected.insert(ordinal) => {}
            (HistoryExportRole::ReplacedSource, None) => {}
            _ => return Err(malformed()),
        }
        match (&node.payload_json, tombstone) {
            (Some(body), None) if serde_json::from_str::<serde_json::Value>(body).is_ok() => {}
            (None, Some(reason)) if !reason.is_empty() => {}
            _ => return Err(malformed()),
        }
        links += relations.previous.len()
            + relations.next.len()
            + relations.replaced_by.len()
            + relations.replacement_groups.len();
        links += relations
            .replacement_groups
            .iter()
            .map(|group| group.sources.len())
            .sum::<usize>();
        if links > MAX_LINKS {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                format!("Thread import supports at most {MAX_LINKS} graph references"),
            ));
        }
    }
    if selected.iter().copied().ne(0..selected.len() as i64) {
        return Err(malformed());
    }
    let linked = |reference: &HistoryExportReference| {
        !reference.id.is_empty()
            && (reference.scope == HistoryExportReferenceScope::Internal)
                == graph.contains_key(reference.id.as_str())
    };
    let internal = |reference: &&HistoryExportReference| {
        reference.scope == HistoryExportReferenceScope::Internal
    };
    let mut groups = HashSet::new();
    let mut incoming = HashMap::new();
    for (&id, relations) in &graph {
        let mut degree = 0usize;
        for (refs, reverse) in [(&relations.previous, true), (&relations.next, false)] {
            let mut seen = HashSet::new();
            for reference in refs {
                if !linked(reference) || !seen.insert(&reference.id) {
                    return Err(malformed());
                }
                if let Some(peer) = graph.get(reference.id.as_str()) {
                    let peers = if reverse { &peer.next } else { &peer.previous };
                    if !peers.iter().filter(internal).any(|item| item.id == id) {
                        return Err(malformed());
                    }
                    degree += usize::from(reverse);
                }
            }
        }
        incoming.insert(id, degree);
        for group in &relations.replacement_groups {
            if group.id.is_empty()
                || !groups.insert(&group.id)
                || !matches!(group.state.as_str(), "completed" | "pruned")
            {
                return Err(malformed());
            }
            let mut ordinal = -1;
            for source in &group.sources {
                if source.ordinal <= ordinal || !linked(&source.node) {
                    return Err(malformed());
                }
                ordinal = source.ordinal;
                if let Some(peer) = graph.get(source.node.id.as_str())
                    && !peer.replaced_by.iter().any(|member| {
                        member.group_id == group.id
                            && member.ordinal == ordinal
                            && member.output.id == id
                    })
                {
                    return Err(malformed());
                }
            }
        }
        let mut memberships = HashSet::new();
        for member in &relations.replaced_by {
            if member.group_id.is_empty()
                || member.ordinal < 0
                || !linked(&member.output)
                || !memberships.insert((&member.group_id, member.ordinal))
            {
                return Err(malformed());
            }
            if let Some(peer) = graph.get(member.output.id.as_str())
                && !peer.replacement_groups.iter().any(|group| {
                    group.id == member.group_id
                        && group
                            .sources
                            .iter()
                            .any(|source| source.ordinal == member.ordinal && source.node.id == id)
                })
            {
                return Err(malformed());
            }
        }
    }
    // Verify the same DAG the storage edge trigger will enforce, before a write.
    let mut ready: Vec<_> = incoming
        .iter()
        .filter_map(|(&id, &degree)| (degree == 0).then_some(id))
        .collect();
    let mut visited = 0;
    while let Some(id) = ready.pop() {
        visited += 1;
        for reference in graph[id].next.iter().filter(internal) {
            let entry = incoming
                .get_mut(reference.id.as_str())
                .expect("validated reference");
            *entry -= 1;
            if *entry == 0 {
                ready.push(reference.id.as_str());
            }
        }
    }
    if visited != graph.len() {
        return Err(malformed());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// Drop signed/private provider material from an outbound copy only.
pub(crate) fn scrub_provider_metadata(value: &mut serde_json::Value) {
    use serde_json::Value;
    fn opaque(value: &Value) -> bool {
        crate::model::opaque::is_opaque(value)
            || value.get("type").and_then(Value::as_str) == Some("redacted_thinking")
    }
    if opaque(value) {
        *value = Value::Null;
        return;
    }
    match value {
        Value::Object(fields) => {
            for key in ["encrypted_content", "signature", "redacted_content"] {
                fields.remove(key);
            }
            fields.retain(|_, value| !opaque(value));
            for child in fields.values_mut() {
                scrub_provider_metadata(child);
            }
        }
        Value::Array(items) => {
            items.retain(|item| !opaque(item));
            for item in items {
                scrub_provider_metadata(item);
            }
        }
        _ => {}
    }
}
