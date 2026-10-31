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
        "Shellboy context notice: {location} was omitted after {reason}. {count} source nodes ({first_id} through {last_id}); projected input before reduction: {before} characters; {spilled} tool groups spilled. The active Turn's initial user request (if any) and the latest group remain.{work} Executed actions, file changes, and command side effects have NOT been undone. Inspect the current state before repeating any earlier operation. Original data is retained and can stay omitted. Search only what you need with shellboy rag search --scope history:{} --mode keyword --query TEXT or shellboy history get --node <source_node_id> with a bounded limit. Only to bring key facts back into context, use shellboy compact --summary --from {first_id} --to {last_id}. To restore this specific range use shellboy compact --restore --from {first_id}; restoring or summarizing omitted history can enlarge context and reproduce the failure.{semantic}{attachment}",
        details.thread
    );
    json!({"item":{"role":"developer","content":[{"type":"input_text","text":notice}]},"context_trim":{"version":1,"omitted_nodes":count,"history_characters":before,"spilled_groups":spilled,"reason":details.reason,"first_node_id":first_id,"last_node_id":last_id,"after_current_request_nodes":details.after_request_nodes,"excluded_attachment_refs":details.excluded_attachment_refs}})
}
/// Pure maximum-omission policy: calculate each retained projection once.
fn disconnect_plan(snapshot: &Snapshot, reason: &str, semantic_search: bool) -> TrimPlan {
    let latest_tool = snapshot.groups.iter().rposition(|g| g.complete && g.tool);
    let request = protected_request(snapshot);
    let mut omitted = HashSet::new();
    let mut first_offset = None;
    let mut offset = 0usize;
    let mut provenance_limited = false;
    for (index, group) in snapshot.groups.iter().enumerate() {
        let end = offset.saturating_add(group.sources.len());
        let eligible = Some(index) != latest_tool
            && !range::trim_group_protected(index, snapshot.groups.len(), request)
            && range::group_is_safe(snapshot, index);
        if eligible {
            let start = first_offset.unwrap_or(offset);
            if end.saturating_sub(start) <= TURN_BOUNDARY_IDS {
                first_offset = Some(start);
                omitted.insert(index);
            } else {
                provenance_limited = true;
                // A single over-budget group cannot be split. If nothing is
                // selected yet, keep looking for a later whole-group window.
                if first_offset.is_some() {
                    break;
                }
            }
        }
        offset = end;
    }
    let before = history_characters(snapshot);
    let marker = marker_payload(snapshot, &omitted, before, 0, reason, semantic_search);
    let mut plan = TrimPlan {
        spills: Vec::new(),
        omitted,
        marker,
        after: before,
        provenance_limited,
    };
    plan.after = history_characters(&projected(snapshot, &plan));
    plan
}

impl Compactor {
    pub(super) async fn trim_maximal(
        &self,
        snapshot: Snapshot,
        cancel: CancellationToken,
        reason: &str,
    ) -> Result<ContextEditResult, ApiError> {
        let before = history_characters(&snapshot);
        let plan = disconnect_plan(&snapshot, reason, semantic_search_configured(&self.config));
        if plan.omitted.is_empty() || plan.after >= before {
            return Ok(ContextEditResult {
                compact: CompactResult { no_target: true, ..Default::default() },
                before_characters: before, after_characters: before,
                warnings: vec!["No safe groups outside the current request, latest complete tool group and protected work".into()],
                ..Default::default()
            });
        }
        self.adopt_trim(snapshot, plan, before, cancel, true).await
    }
    pub(super) async fn trim_before(
        &self,
        snapshot: Snapshot,
        to_node: &str,
        cancel: CancellationToken,
    ) -> Result<ContextEditResult, ApiError> {
        let selected = range::from_prefix_end(&snapshot, to_node)?;
        self.omit_range(snapshot, selected, cancel).await
    }
    /// Prefix trimming and a middle shortcut use the same validated operation.
    pub(super) async fn omit_range(
        &self,
        snapshot: Snapshot,
        selected: Range<usize>,
        cancel: CancellationToken,
    ) -> Result<ContextEditResult, ApiError> {
        range::validate(
            &snapshot,
            std::slice::from_ref(&selected),
            range::RangeMode::Trim,
        )?;
        let omitted: HashSet<_> = selected.collect();
        let before = history_characters(&snapshot);
        let marker = marker_payload(
            &snapshot,
            &omitted,
            before,
            0,
            "manual",
            semantic_search_configured(&self.config),
        );
        let mut plan = TrimPlan {
            spills: Vec::new(),
            omitted,
            marker,
            after: 0,
            provenance_limited: false,
        };
        plan.after = history_characters(&projected(&snapshot, &plan));
        self.adopt_trim(snapshot, plan, before, cancel, false).await
    }
    async fn adopt_trim(
        &self,
        snapshot: Snapshot,
        plan: TrimPlan,
        before: u64,
        cancel: CancellationToken,
        automatic: bool,
    ) -> Result<ContextEditResult, ApiError> {
        let ranges = omission_ranges(&plan.omitted, snapshot.groups.len());
        range::validate(&snapshot, &ranges, range::RangeMode::Trim)?;
        let spill_ranges: Vec<_> = plan
            .spills
            .iter()
            .filter(|(index, _)| !plan.omitted.contains(index))
            .map(|(index, _)| *index..*index + 1)
            .collect();
        range::validate(&snapshot, &spill_ranges, range::RangeMode::Spill)?;
        let first_changed = plan
            .omitted
            .iter()
            .copied()
            .chain(plan.spills.iter().map(|(index, _)| *index))
            .min()
            .unwrap_or(0);
        let cache_invalidated = suffix_characters(&snapshot, first_changed);
        let source_count: usize = snapshot
            .groups
            .iter()
            .enumerate()
            .filter(|(index, _)| plan.omitted.contains(index))
            .map(|(_, group)| group.sources.len())
            .sum();
        let original_span_count = plan
            .marker
            .as_ref()
            .and_then(|payload| payload.pointer("/context_trim/original_order"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if source_count > TURN_BOUNDARY_IDS || original_span_count > TURN_BOUNDARY_IDS {
            return Err(invalid("Context omission exceeds the bounded restore provenance budget").hint("Choose a smaller whole-group range (at most 65536 source and retained-anchor nodes)"));
        }
        if !plan.omitted.is_empty() && plan.marker.is_none() {
            return Err(invalid("Context omission has no valid replacement marker"));
        }
        let spill_payloads: HashMap<_, _> = plan
            .spills
            .iter()
            .map(|(index, payload)| (*index, payload.clone()))
            .collect();
        self.database.call(move |db| {
            let tx = db.transaction()?;
            if cancel.is_cancelled() { return Err(StorageError::from(invalid("Context edit cancelled before adoption"))); }
            validate(&tx, &snapshot)?;
            let sources: Vec<_> = snapshot.groups.iter().enumerate().filter(|(index, _)| plan.omitted.contains(index)).flat_map(|(_, group)| group.sources.clone()).collect();
            let omitted_refs=sources.iter().collect::<Vec<_>>();
            let changed_refs=snapshot.groups.iter().enumerate().filter(|(index,_)|plan.omitted.contains(index)||spill_payloads.contains_key(index)).flat_map(|(_,group)|group.sources.iter()).collect::<Vec<_>>();
            let expected_omitted=snapshot.groups.iter().enumerate().filter(|(index,_)|plan.omitted.contains(index)).map(|(_,group)|active_attachments(group)).sum::<usize>();
            let expected_changed=snapshot.groups.iter().enumerate().filter(|(index,_)|plan.omitted.contains(index)||spill_payloads.contains_key(index)).map(|(_,group)|active_attachments(group)).sum::<usize>();
            let omitted_attachments=count_active_attachments_tx(&tx,&snapshot.thread,&omitted_refs)?;
            let changed_attachments=count_active_attachments_tx(&tx,&snapshot.thread,&changed_refs)?;
            if omitted_attachments!=expected_omitted || changed_attachments!=expected_changed {
                return Err(StorageError::from(invalid("Context edit attachment selection changed; original path retained")));
            }
            let marker = plan.marker.as_ref().map(|payload| store_replacement(&tx, &snapshot, &sources, payload, "context-trim-v1")).transpose()?;
            let mut path = Vec::new();
            let mut marked = false;
            let mut spilled = 0;
            let mut original_ids: Vec<_> = sources.iter().map(|source| source.id.clone()).collect();
            for (index, group) in snapshot.groups.iter().enumerate() {
                if plan.omitted.contains(&index) {
                    if !marked { if let Some(marker) = &marker { path.push(marker.clone()); } marked = true; }
                } else if let Some(payload) = spill_payloads.get(&index) {
                    path.push(store_replacement(&tx, &snapshot, &group.sources, payload, "spill-v1")?);
                    spilled += 1;
                    original_ids.extend(group.sources.iter().map(|source| source.id.clone()));
                } else { path.extend(group.sources.iter().map(|source| source.id.clone())); }
            }
            let mut warnings = Vec::new();
            if changed_attachments > 0 {
                warnings.push(format!("{changed_attachments} active attachment references were excluded by this context edit and will not be automatically reactivated by restore"));
            }
            if plan.provenance_limited {
                warnings.push("Automatic omission retained additional candidates beyond the oldest whole-group window allowed by the 65536-node restore provenance budget".into());
            }
            let result = ContextEditResult { compact: CompactResult { spilled_groups: spilled, source_nodes: original_ids.len(), original_node_ids: original_ids, ..Default::default() }, before_characters: before, after_characters: plan.after,  omitted_nodes: sources.len(), marker_node_id: marker, changed_suffix_characters: cache_invalidated, warnings };
            history::select_path(&tx, &snapshot.thread, snapshot.revision, &path, &timestamp())?;
            if automatic && let Some((turn, generation)) = &snapshot.turn_fence {
                // Make the reason durable with the path change, even if owner
                // exits before Driver records the failed transport attempt.
                tx.execute("UPDATE model_attempt SET error_json=json_set(COALESCE(error_json,'{}'),'$.automatic_trim',json(?1)) WHERE turn_id=?2 AND generation=?3 AND state='inflight'", params![serde_json::to_string(&result).expect("context result"),turn,generation])?;
            }
            tx.commit()?;
            Ok(result)
        }).await.map_err(storage_error)
    }
}

impl Compactor {
    /// Saved/projection facts only. No estimate, diagnosis or automatic action.
    pub(crate) async fn recovery_facts(&self, thread: &str) -> Result<Value, ApiError> {
        let snapshot = self.snapshot(thread, true).await?;
        let request = protected_request(&snapshot);
        let tool = snapshot.groups.iter().rposition(|g| g.complete && g.tool);
        let sizes = |index: Option<usize>| {
            let mut chars = 0u64;
            let mut bytes = 0u64;
            if let Some(group) = index.and_then(|i| snapshot.groups.get(i)) {
                for source in &group.sources {
                    for item in
                        crate::control::replay::convert(source.payload.clone(), snapshot.epoch)
                    {
                        let body = item.to_string();
                        chars = chars.saturating_add(body.chars().count() as u64);
                        bytes = bytes.saturating_add(body.len() as u64);
                    }
                }
            }
            json!({"characters":chars,"bytes":bytes})
        };
        let plan = disconnect_plan(
            &snapshot,
            "manual_maximal",
            semantic_search_configured(&self.config),
        );
        let reducible = !plan.omitted.is_empty() && plan.after < history_characters(&snapshot);
        let restored = snapshot
            .groups
            .iter()
            .any(|g| g.sources.iter().any(|s| marker_metadata(s).is_some()));
        let thread = thread.to_owned();
        let attachments = self.database.call(move |db| {
            let mut stmt = db.prepare("SELECT a.id,(SELECT raw_size FROM attachment_send WHERE attachment_id=a.id ORDER BY rowid DESC LIMIT 1)
              FROM attachment_selection s JOIN attachment_ref a ON a.id=s.attachment_id
              WHERE s.thread_id=?1 AND s.state='active' ORDER BY s.selected_order LIMIT 201")?;
            let rows=stmt.query_map([thread],|r| Ok(json!({"id":r.get::<_,String>(0)?,"bytes":r.get::<_,Option<i64>>(1)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await.map_err(storage_error)?;
        let mut actions = vec!["/retry", "/edit", "/stop"];
        if reducible {
            actions.push("/compact-max");
        }
        if !attachments.is_empty() {
            actions.push("/detach");
        }
        if restored {
            actions.push("/restore");
        }
        // /model opens the existing selection screen, not a Core recommendation.
        if self.config.connections.len() > 1 {
            actions.push("/model");
        }
        Ok(
            json!({"protected_request":sizes(request),"protected_latest_tool":sizes(tool),
            "attachments":attachments,"attachments_partial":attachments.len()>200,
            "maximum_reduction_available":reducible,"actions":actions}),
        )
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn oversized_automatic_history_uses_an_available_bounded_window_and_retains_anchors() {
        let mut snapshot = crate::compaction::tests::staged_snapshot();
        let mut prototype = snapshot.groups[0].clone();
        prototype.sources[0].payload = json!({"role":"user","content":"small"});
        prototype.sources[0].turn = None;
        snapshot.current_turn = None;
        snapshot.protected.clear();
        snapshot.groups = (0..TURN_BOUNDARY_IDS + 16)
            .map(|index| {
                let mut group = prototype.clone();
                group.sources[0].id = format!("node-{index}");
                group
            })
            .collect();
        snapshot.protected.insert("node-1".into());
        let plan = disconnect_plan(&snapshot, "manual_maximal", false);
        assert!(plan.provenance_limited);
        assert_eq!(plan.omitted.len(), TURN_BOUNDARY_IDS - 1);
        assert!(plan.omitted.contains(&0));
        assert!(!plan.omitted.contains(&1));
        assert!(!plan.omitted.contains(&TURN_BOUNDARY_IDS));
        let marker = plan.marker.unwrap();
        assert_eq!(
            marker["context_trim"]["original_order"]
                .as_array()
                .unwrap()
                .len(),
            TURN_BOUNDARY_IDS
        );
        assert_eq!(
            marker["context_trim"]["retained_node_ids"],
            json!(["node-1"])
        );
        assert_eq!(
            marker["source_node_ids"].as_array().unwrap().len(),
            TURN_BOUNDARY_IDS - 1
        );
    }
}
