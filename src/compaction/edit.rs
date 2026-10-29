//! Explicit selected-path edits and deterministic recovery share the same
//! immutable sources, range validation, and transactional adoption machinery.
use super::*;
use crate::protocol::{ContextEditAction, ContextEditArgs};
#[cfg(test)]
mod tests;
mod trim;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextEditResult {
    pub compact: CompactResult,
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub target_tokens: Option<u64>,
    pub omitted_nodes: usize,
    pub marker_node_id: Option<String>,
    pub cache_invalidated_tokens: u64,
    pub warnings: Vec<String>,
}
mod estimate;
pub(super) use estimate::{
    TokenEstimate, estimated_suffix_tokens, estimated_tokens, history_characters,
};
fn required_endpoint(value: Option<&str>) -> Result<&str, ApiError> {
    value.ok_or_else(|| edit_error("Context edit requires complete node endpoints"))
}
fn edit_error(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}
fn marker_metadata(source: &Source) -> Option<&Value> {
    source
        .payload
        .get("context_trim")
        .filter(|value| value.is_object())
}
fn node_ids(value: &Value, field: &str) -> Result<Vec<String>, ApiError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .and_then(|ids| {
            ids.iter()
                .map(|id| id.as_str().map(str::to_owned))
                .collect()
        })
        .ok_or_else(|| invalid("Context marker has invalid source ordering metadata"))
}
impl Compactor {
    /// Activation uses prepared request counts and bounded usage calibration,
    /// without materializing the selected path or querying node attachments.
    pub(crate) async fn input_tokens(
        &self,
        thread: &str,
        profile: &str,
        model: &str,
        characters: u64,
    ) -> Result<u64, ApiError> {
        let (thread, profile, model) = (thread.to_owned(), profile.to_owned(), model.to_owned());
        self.database
            .call(move |db| {
                Ok(TokenEstimate::calibration(db, &thread, &profile, &model)?.tokens(characters))
            })
            .await
            .map_err(storage_error)
    }
    pub(crate) async fn trim_activated(
        &self,
        thread: &str,
        cancel: CancellationToken,
    ) -> Result<ContextEditResult, ApiError> {
        let snapshot = self.snapshot(thread, true).await?;
        self.trim_maximal(snapshot, cancel).await
    }
    pub async fn edit(
        &self,
        thread: &str,
        args: &ContextEditArgs,
        cancel: CancellationToken,
    ) -> Result<ContextEditResult, ApiError> {
        args.validate()?;
        let snapshot = self
            .snapshot(
                thread,
                matches!(
                    args.action,
                    ContextEditAction::Trim | ContextEditAction::Omit
                ),
            )
            .await?;
        let before = estimated_tokens(&snapshot);
        let target = self
            .config
            .connections
            .get(&snapshot.profile)
            .and_then(|connection| connection.compact_hint_tokens)
            .unwrap_or(75_000);
        match args.action {
            ContextEditAction::Trim => {
                self.trim_before(
                    snapshot,
                    required_endpoint(args.to_node_id.as_deref())?,
                    cancel,
                )
                .await
            }
            ContextEditAction::Omit => {
                let selected = range::from_nodes_for_mode(
                    &snapshot,
                    required_endpoint(args.from_node_id.as_deref())?,
                    required_endpoint(args.to_node_id.as_deref())?,
                    range::RangeMode::Trim,
                )?;
                self.omit_range(snapshot, selected, cancel).await
            }
            ContextEditAction::Restore => {
                self.restore_marker(snapshot, args.from_node_id.as_deref(), cancel)
                    .await
            }
            ContextEditAction::Spill | ContextEditAction::Summary => {
                let from = required_endpoint(args.from_node_id.as_deref())?;
                let to = required_endpoint(args.to_node_id.as_deref())?;
                let selected = match range::from_nodes(&snapshot, from, to) {
                    Ok(selected) => selected,
                    Err(error) if matches!(args.action, ContextEditAction::Summary) => {
                        return self
                            .summarize_hidden_marker(snapshot, from, to, cancel, error)
                            .await;
                    }
                    Err(error) => return Err(error),
                };
                if matches!(args.action, ContextEditAction::Summary)
                    && selected.end == selected.start + 1
                    && snapshot.groups[selected.start].sources.len() == 1
                    && marker_metadata(&snapshot.groups[selected.start].sources[0]).is_some()
                {
                    return self
                        .summarize_hidden_marker(
                            snapshot,
                            from,
                            to,
                            cancel,
                            edit_error("Selected marker cannot be summarized"),
                        )
                        .await;
                }
                let invalidated = estimated_suffix_tokens(&snapshot, selected.start);
                let compact = if matches!(args.action, ContextEditAction::Spill) {
                    self.spill_snapshot(snapshot, vec![selected], cancel)
                        .await?
                } else {
                    self.summarize_snapshot(snapshot, selected, cancel).await?
                };
                let after = estimated_tokens(&self.snapshot(thread, false).await?);
                Ok(ContextEditResult {
                    compact,
                    before_tokens: before,
                    after_tokens: after,
                    target_tokens: Some(target),
                    cache_invalidated_tokens: invalidated,
                    ..Default::default()
                })
            }
        }
    }
    pub async fn context_metrics(&self, thread: &str) -> Result<Value, ApiError> {
        let snapshot = self.snapshot(thread, false).await?;
        let estimate = estimated_tokens(&snapshot);
        let thread = thread.to_owned();
        let profile = snapshot.profile.clone();
        let model = snapshot.model.clone();
        let latest=self.database.call(move|db| {
            let result=db.query_row("SELECT a.id,CASE WHEN length(CAST(a.usage_json AS BLOB))<=65536 THEN a.usage_json END,CASE WHEN length(CAST(a.input_manifest_json AS BLOB))<=65536 THEN a.input_manifest_json END FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 AND a.state='completed' AND a.profile_id=?2 AND a.model_id=?3 ORDER BY a.rowid DESC LIMIT 1",params![thread,profile,model],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,Option<String>>(2)?))).optional()?;
            Ok(result.map(|(attempt,usage,manifest)| {
                let usage=usage.filter(|body|body.len()<=64*1024).and_then(|body|serde_json::from_str::<Value>(&body).ok()).unwrap_or(Value::Null);
                let manifest=manifest.and_then(|body|serde_json::from_str::<Value>(&body).ok()).unwrap_or(Value::Null);
                json!({"attempt_id":attempt,"input_tokens":usage.get("input_tokens").and_then(Value::as_u64),"cached_tokens":usage.pointer("/input_tokens_details/cached_tokens").and_then(Value::as_u64),"input_characters":manifest.get("input_characters").and_then(Value::as_u64)})
            }))
        }).await.map_err(storage_error)?;
        Ok(
            json!({"estimated_input_tokens":estimate,"compact_hint_tokens":self.config.connections.get(&snapshot.profile).and_then(|connection|connection.compact_hint_tokens),"effective_activation_tokens":self.config.connections.get(&snapshot.profile).and_then(|connection|connection.compact_hint_tokens).unwrap_or(75_000),"last_successful_attempt":latest,"estimation_method":snapshot.estimate.method(),"estimation_baseline_attempt_id":snapshot.estimate.attempt_id,"estimated_history_characters":history_characters(&snapshot),"estimated_overhead_characters":snapshot.estimate.overhead(),"estimate_excludes":if snapshot.estimate.overhead().is_some(){Vec::<&str>::new()}else{vec!["unmeasured_system_instructions","unmeasured_attachment_binary_projection"]}}),
        )
    }
    async fn replacement_sources(
        &self,
        snapshot: &Snapshot,
        marker: &Source,
    ) -> Result<(Vec<Source>, HashSet<String>), ApiError> {
        let marker = marker.id.clone();
        let thread = snapshot.thread.clone();
        self.database.call(move|db| {
            let rows=db.prepare("SELECT n.id,n.turn_id,n.payload_revision,n.payload_json,s.source_revision,s.source_hash FROM replacement r JOIN replacement_source s ON s.replacement_id=r.id JOIN node n ON n.id=s.source_node_id WHERE r.output_node_id=?1 AND r.state='completed' ORDER BY s.ordinal LIMIT ?2")?.query_map(params![marker,(TURN_BOUNDARY_IDS+1) as i64],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,i64>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,i64>(4)?,row.get::<_,String>(5)?)))?.collect::<Result<Vec<_>,_>>()?;
            if rows.is_empty()||rows.len()>TURN_BOUNDARY_IDS {return Err(StorageError::from(invalid("Context marker source list is unavailable or exceeds the bounded provenance budget")));}
            let mut sources=Vec::new();let mut completed=HashSet::new();let mut seen=HashSet::new();
            for (id,turn,revision,body,saved_revision,saved_hash) in rows {
                let payload:Value=body.and_then(|body|serde_json::from_str(&body).ok()).ok_or_else(||StorageError::from(invalid("A context marker source is unavailable; selected path retained")))?;
                let hash=key(&(revision,&payload));
                if revision!=saved_revision||hash!=saved_hash {return Err(StorageError::from(invalid("A context marker source changed; selected path retained")));}
                if let Some(group)=payload.get("group_id").and_then(Value::as_str)&&seen.insert(group.to_owned()) {
                    let complete:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM tool_group g WHERE g.id=?1 AND g.state='completed' AND NOT EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state!='completed' OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id))))",[group],|row|row.get(0))?;
                    if complete {completed.insert(group.to_owned());}
                }
                let attachments=db.prepare("SELECT a.id,a.absolute_path,a.input_kind,COALESCE(s.state,'not_selected'),s.reason FROM attachment_ref a LEFT JOIN attachment_selection s ON s.attachment_id=a.id AND s.thread_id=?2 WHERE a.source_node_id=?1 ORDER BY a.created_at,a.id")?.query_map(params![id,thread],|row|Ok(json!({"id":row.get::<_,String>(0)?,"path":row.get::<_,String>(1)?,"kind":row.get::<_,String>(2)?,"state":row.get::<_,String>(3)?,"reason":row.get::<_,Option<String>>(4)?})))?.collect::<Result<Vec<_>,_>>()?;
                sources.push(Source{id,turn,revision,hash,payload,attachments});
            }
            Ok((sources,completed))
        }).await.map_err(storage_error)
    }
    async fn summarize_hidden_marker(
        &self,
        mut snapshot: Snapshot,
        from: &str,
        to: &str,
        cancel: CancellationToken,
        original_error: ApiError,
    ) -> Result<ContextEditResult, ApiError> {
        let before = estimated_tokens(&snapshot);
        let target = self
            .config
            .connections
            .get(&snapshot.profile)
            .and_then(|connection| connection.compact_hint_tokens)
            .unwrap_or(75_000);
        let markers = snapshot
            .groups
            .iter()
            .enumerate()
            .filter_map(|(index, group)| {
                group
                    .sources
                    .first()
                    .filter(|source| group.sources.len() == 1 && marker_metadata(source).is_some())
                    .map(|source| (index, source.clone()))
            })
            .collect::<Vec<_>>();
        let omitted_ranges = markers.iter().filter_map(|(_,marker)| {
            let ids=marker.payload.get("source_node_ids")?.as_array()?;
            Some(json!({"marker_node_id":marker.id,"from_node_id":ids.first()?.as_str()?,"to_node_id":ids.last()?.as_str()?}))
        }).take(16).collect::<Vec<_>>();
        for (index, marker) in markers {
            let by_marker = marker.id == from && marker.id == to;
            let ids = marker
                .payload
                .get("source_node_ids")
                .and_then(Value::as_array);
            if !by_marker
                && !ids.is_some_and(|ids| {
                    ids.first().and_then(Value::as_str) == Some(from)
                        && ids.last().and_then(Value::as_str) == Some(to)
                })
            {
                continue;
            }
            range::validate(
                &snapshot,
                std::slice::from_ref(&(index..index + 1)),
                range::RangeMode::Summary,
            )?;
            let (sources, completed) = self.replacement_sources(&snapshot, &marker).await?;
            if !by_marker
                && (sources.first().is_none_or(|source| source.id != from)
                    || sources.last().is_none_or(|source| source.id != to))
            {
                continue;
            }
            let groups = source_groups(sources, &completed);
            let count = groups.len();
            let invalidated = estimated_suffix_tokens(&snapshot, index);
            snapshot.extra_sources.push(marker);
            snapshot.groups.splice(index..index + 1, groups);
            let thread = snapshot.thread.clone();
            let compact = self
                .summarize_snapshot(snapshot, index..index + count, cancel)
                .await?;
            let after = estimated_tokens(&self.snapshot(&thread, false).await?);
            return Ok(ContextEditResult {
                compact,
                before_tokens: before,
                after_tokens: after,
                target_tokens: Some(target),
                cache_invalidated_tokens: invalidated,
                ..Default::default()
            });
        }
        let mut details = original_error.details.clone().unwrap_or_else(|| json!({}));
        if let Some(details) = details.as_object_mut() {
            details.insert("omitted_ranges".into(), json!(omitted_ranges));
        }
        Err(original_error.details(details).hint("For omitted history, select the complete first-to-last source range of one context marker"))
    }
    async fn restore_marker(
        &self,
        snapshot: Snapshot,
        marker_id: Option<&str>,
        cancel: CancellationToken,
    ) -> Result<ContextEditResult, ApiError> {
        let before = estimated_tokens(&snapshot);
        let target = self
            .config
            .connections
            .get(&snapshot.profile)
            .and_then(|connection| connection.compact_hint_tokens)
            .unwrap_or(75_000);
        // "Latest" means the most recently created selected edit, not the
        // marker nearest the end of the conversation. An earlier range may
        // have been omitted after a later one.
        let marker_id = if let Some(requested) = marker_id {
            let candidates = snapshot
                .groups
                .iter()
                .filter(|group| group.sources.len() == 1)
                .filter_map(|group| group.sources.first())
                .filter(|source| marker_metadata(source).is_some())
                .collect::<Vec<_>>();
            if candidates.iter().any(|source| source.id == requested) {
                Some(requested.to_owned())
            } else {
                let matching = candidates
                    .iter()
                    .filter(|source| {
                        source
                            .payload
                            .pointer("/context_trim/first_node_id")
                            .and_then(Value::as_str)
                            == Some(requested)
                    })
                    .collect::<Vec<_>>();
                match matching.as_slice() {
                    []=>None,
                    [source]=>Some(source.id.clone()),
                    _=>return Err(edit_error("Restore source ID matches multiple selected markers; use an explicit marker ID").details(json!({"marker_node_ids":matching.iter().take(16).map(|source|&source.id).collect::<Vec<_>>()}))),
                }
            }
        } else {
            let thread = snapshot.thread.clone();
            self.database.call(move |db| {
                Ok(db.query_row("SELECT n.id FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND json_type(n.payload_json,'$.context_trim')='object' ORDER BY n.rowid DESC LIMIT 1", [thread], |row| row.get::<_, String>(0)).optional()?)
            }).await.map_err(storage_error)?
        };
        let found = snapshot
            .groups
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, group)| {
                group
                    .sources
                    .first()
                    .filter(|source| {
                        group.sources.len() == 1
                            && marker_metadata(source).is_some()
                            && marker_id.as_deref().is_some_and(|id| source.id == id)
                    })
                    .map(|source| (index, source.clone()))
            });
        let Some((index, marker)) = found else {
            return Err(edit_error(
                "No selected context marker matches the restore request",
            ));
        };
        range::validate(
            &snapshot,
            std::slice::from_ref(&(index..index + 1)),
            range::RangeMode::Summary,
        )?;
        let (originals, completed) = self.replacement_sources(&snapshot, &marker).await?;
        let metadata = marker_metadata(&marker)
            .ok_or_else(|| invalid("Selected node is not a context marker"))?;
        let original_order = if metadata.get("original_order").is_some() {
            node_ids(metadata, "original_order")?
        } else {
            originals.iter().map(|source| source.id.clone()).collect()
        };
        let retained = metadata
            .get("retained_node_ids")
            .map(|_| node_ids(metadata, "retained_node_ids"))
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .collect::<HashSet<_>>();
        let selected = snapshot
            .groups
            .iter()
            .flat_map(|group| &group.sources)
            .collect::<Vec<_>>();
        let marker_position = selected
            .iter()
            .position(|source| source.id == marker.id)
            .ok_or_else(|| invalid("Selected marker changed"))?;
        let mut restored = originals
            .iter()
            .map(|source| (source.id.clone(), source.clone()))
            .collect::<HashMap<_, _>>();
        for id in &retained {
            let source = selected
                .iter()
                .enumerate()
                .find(|(position, source)| *position > marker_position && source.id == *id)
                .map(|(_, source)| (*source).clone())
                .ok_or_else(|| {
                    invalid("A retained marker anchor is missing or moved; selected path retained")
                })?;
            restored.insert(id.clone(), source);
        }
        if original_order.len() != restored.len()
            || original_order.iter().collect::<HashSet<_>>().len() != original_order.len()
        {
            return Err(invalid(
                "Context marker source ordering is incomplete or duplicated",
            ));
        }
        let expanded = original_order
            .iter()
            .map(|id| {
                restored
                    .remove(id)
                    .ok_or_else(|| invalid("Context marker contains an unknown source ID"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let excluded_attachments = originals
            .iter()
            .flat_map(|source| &source.attachments)
            .filter(|attachment| {
                attachment.get("state").and_then(Value::as_str) == Some("excluded")
            })
            .count();
        let mut validation = snapshot.clone();
        validation.groups = source_groups(originals, &completed);
        if !validation.groups.is_empty() {
            range::validate(
                &validation,
                std::slice::from_ref(&(0..validation.groups.len())),
                range::RangeMode::Summary,
            )?;
        }
        let mut path = selected[..marker_position]
            .iter()
            .map(|source| source.id.clone())
            .collect::<Vec<_>>();
        path.extend(expanded.iter().map(|source| source.id.clone()));
        path.extend(
            selected[marker_position + 1..]
                .iter()
                .filter(|source| !retained.contains(&source.id))
                .map(|source| source.id.clone()),
        );
        let invalidated = estimated_suffix_tokens(&snapshot, index);
        let mut after_snapshot = snapshot.clone();
        let suffix = snapshot.groups[index + 1..]
            .iter()
            .flat_map(|group| {
                group
                    .sources
                    .iter()
                    .filter(|source| !retained.contains(&source.id))
                    .cloned()
            })
            .collect::<Vec<_>>();
        let mut all = snapshot.groups[..index]
            .iter()
            .flat_map(|group| group.sources.clone())
            .collect::<Vec<_>>();
        all.extend(expanded);
        all.extend(suffix);
        after_snapshot.groups = source_groups(all, &completed);
        let after = estimated_tokens(&after_snapshot);
        let mut warnings = Vec::new();
        if excluded_attachments > 0 {
            warnings.push(format!("Restored text only: {excluded_attachments} excluded attachment references remain excluded; attach again with new IDs if needed"));
        }
        if target > 0 && after > target {
            warnings.push(
                "Restored context exceeds compact_hint_tokens; consider a focused summary".into(),
            );
        }
        let result = ContextEditResult {
            compact: CompactResult {
                source_nodes: validation
                    .groups
                    .iter()
                    .map(|group| group.sources.len())
                    .sum(),
                original_node_ids: original_order,
                ..Default::default()
            },
            before_tokens: before,
            after_tokens: after,
            target_tokens: Some(target),
            marker_node_id: Some(marker.id),
            cache_invalidated_tokens: invalidated,
            warnings,
            ..Default::default()
        };
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                if cancel.is_cancelled() {
                    return Err(StorageError::from(invalid(
                        "Context restore cancelled before adoption",
                    )));
                }
                validate(&tx, &snapshot)?;
                validate(&tx, &validation)?;
                history::select_path(
                    &tx,
                    &snapshot.thread,
                    snapshot.revision,
                    &path,
                    &timestamp(),
                )?;
                tx.commit()?;
                Ok(result)
            })
            .await
            .map_err(storage_error)
    }
}

#[cfg(test)]
mod summary_tests;

#[cfg(test)]
mod user_send_tests;
