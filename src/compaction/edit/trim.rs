//! Plan first, then adopt deterministic reductions in one fenced transaction.
use super::*;

struct TrimPlan {
    spills: Vec<(usize, Value)>,
    omitted: HashSet<usize>,
    marker: Option<Value>,
    after: u64,
    provenance_limited: bool,
}

fn protected_request(snapshot: &Snapshot) -> Option<usize> {
    policy::request_boundary(snapshot, snapshot.current_turn.as_deref())
}
fn omission_ranges(omitted: &HashSet<usize>, count: usize) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < count {
        if !omitted.contains(&index) {
            index += 1;
            continue;
        }
        let start = index;
        while index < count && omitted.contains(&index) {
            index += 1;
        }
        ranges.push(start..index);
    }
    ranges
}
fn projected(snapshot: &Snapshot, plan: &TrimPlan) -> Snapshot {
    let mut reduced = snapshot.clone();
    reduced.groups.clear();
    let mut marked = false;
    for (index, group) in snapshot.groups.iter().enumerate() {
        if plan.omitted.contains(&index) {
            if !marked && let Some(marker) = &plan.marker {
                let mut source = group.sources[0].clone();
                source.payload = marker.clone();
                reduced.groups.push(Group {
                    sources: vec![source],
                    complete: true,
                    tool: false,
                });
                marked = true;
            }
        } else if let Some((_, payload)) = plan.spills.iter().find(|(ordinal, _)| *ordinal == index)
        {
            let mut source = group.sources[0].clone();
            source.payload = payload.clone();
            reduced.groups.push(Group {
                sources: vec![source],
                complete: true,
                tool: false,
            });
        } else {
            reduced.groups.push(group.clone());
        }
    }
    reduced
}
#[derive(Clone, Copy)]
struct NoticeDetails<'a> {
    thread: &'a str,
    reason: &'a str,
    after_request_nodes: usize,
    excluded_attachment_refs: usize,
    start_group: usize,
    semantic_search: bool,
}
fn active_attachments(group: &Group) -> usize {
    group
        .sources
        .iter()
        .flat_map(|source| &source.attachments)
        .filter(|attachment| attachment.get("state").and_then(Value::as_str) == Some("active"))
        .count()
}
fn count_active_attachments_tx(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    sources: &[&Source],
) -> Result<usize, StorageError> {
    let ids = serde_json::to_string(&sources.iter().map(|source| &source.id).collect::<Vec<_>>())
        .expect("source IDs");
    let count:i64=tx.query_row("SELECT count(*) FROM attachment_selection s JOIN attachment_ref a ON a.id=s.attachment_id WHERE s.thread_id=?1 AND s.state='active' AND a.source_node_id IN (SELECT value FROM json_each(?2))",params![thread,ids],|row|row.get(0))?;
    Ok(count as usize)
}
fn semantic_search_configured(config: &Config) -> bool {
    config.embedding.connection.is_some() && config.embedding.model.is_some()
}
fn marker_payload(
    snapshot: &Snapshot,
    omitted: &HashSet<usize>,
    before: u64,
    spilled: usize,
    reason: &str,
    semantic_search: bool,
) -> Option<Value> {
    let first = (0..snapshot.groups.len()).find(|index| omitted.contains(index))?;
    let last = (0..snapshot.groups.len()).rfind(|index| omitted.contains(index))?;
    let sources: Vec<_> = (first..=last)
        .filter(|index| omitted.contains(index))
        .flat_map(|index| snapshot.groups[index].sources.iter())
        .collect();
    let original_order: Vec<_> = snapshot.groups[first..=last]
        .iter()
        .flat_map(|group| group.sources.iter().map(|source| &source.id))
        .collect();
    let retained: Vec<_> = (first..=last)
        .filter(|index| !omitted.contains(index))
        .flat_map(|index| {
            snapshot.groups[index]
                .sources
                .iter()
                .map(|source| &source.id)
        })
        .collect();
    let ids: Vec<_> = sources.iter().map(|source| &source.id).collect();
    let first_id = ids.first()?;
    let last_id = ids.last()?;
    let request = protected_request(snapshot);
    let after_request_nodes = omitted
        .iter()
        .filter(|index| request.is_some_and(|request| **index > request))
        .map(|index| snapshot.groups[*index].sources.len())
        .sum();
    let excluded_attachment_refs = omitted
        .iter()
        .map(|index| active_attachments(&snapshot.groups[*index]))
        .sum();
    let mut payload = marker_item(
        first_id,
        last_id,
        ids.len(),
        before,
        spilled,
        NoticeDetails {
            thread: &snapshot.thread,
            reason,
            after_request_nodes,
            excluded_attachment_refs,
            start_group: first,
            semantic_search,
        },
    );
    payload["context_trim"]["original_order"] = json!(original_order);
    payload["context_trim"]["retained_node_ids"] = json!(retained);
    payload["source_node_ids"] = json!(ids);
    Some(payload)
}
fn marker_item(
    first_id: &str,
    last_id: &str,
    count: usize,
    before: u64,
    spilled: usize,
    details: NoticeDetails<'_>,
) -> Value {
    let location = if details.start_group == 0 {
        "a range at the beginning of the selected history"
    } else {
        "a middle range of the selected history"
    };
    let reason = if details.reason == "failure" {
        "a recorded model-request failure or explicit maximum reduction"
    } else {
        "an explicit context edit request"
    };
    let work = if details.after_request_nodes > 0 {
        format!(
            " This includes {} source nodes AFTER the retained current user request: intermediate work in the current Turn was also omitted. Do not assume that work was never performed.",
            details.after_request_nodes
        )
    } else {
        String::new()
    };
    let attachment = if details.excluded_attachment_refs > 0 {
        format!(
            " {} active attachment references were excluded by this marker and will not be reactivated by restore; attach again explicitly with new IDs if needed.",
            details.excluded_attachment_refs
        )
    } else {
        String::new()
    };
    let semantic = if details.semantic_search {
        format!(
            " To find decisions or context with different wording, use shellboy history search --thread {} --query TEXT (semantic search sends a query-embedding API request and may incur cost; configured embeddings do not guarantee index coverage).",
            details.thread
        )
    } else {
        String::new()
    };
    let notice = format!(
        "Shellboy context notice: {location} was omitted after {reason}. {count} source nodes ({first_id} through {last_id}); projected input before reduction: {before} characters; {spilled} tool groups spilled. The active Turn's initial user request (if any) and the latest group remain.{work} Executed actions, file changes, and command side effects have NOT been undone. Inspect the current state before repeating any earlier operation. Original data is retained and can stay omitted. Search only what you need with shellboy rag search --scope history:{} --mode keyword --query TEXT or shellboy history get --node <source_node_id> with a bounded limit. Only to bring key facts back into context, use shellboy compact --summary --from {first_id} --to {last_id} --max-chars 20000 --max-percent 5. To restore this specific range use shellboy compact --restore --from {first_id}; restoring or summarizing omitted history can enlarge context and reproduce the failure.{semantic}{attachment}",
