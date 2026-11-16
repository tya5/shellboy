//! Fixed successful-summary adoption, retaining the caller's one transaction.
use super::discard::{RejectionReason, rejected};
use super::*;
#[derive(Clone, Debug)]
pub(super) struct FinalRSource {
    pub(super) fact: super::gc_capture::GcSourceFacts,
    pub(super) hash: String,
}

fn rejection(reason: RejectionReason, message: &str) -> StorageError {
    StorageError::from(rejected(reason, message))
}

pub(super) fn final_r_source(
    source: &Source,
    group_complete: bool,
    protected: bool,
) -> FinalRSource {
    FinalRSource {
        fact: super::gc_capture::GcSourceFacts {
            ordinal: 0,
            id: source.id.clone(),
            revision: source.revision,
            turn: source.turn.clone(),
            group_id: source
                .payload
                .get("group_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            protected,
            group_complete,
            has_summary_marker: source.payload.get("summary_format").is_some(),
            attachments: source.attachments.clone(),
        },
        hash: source.hash.clone(),
    }
}

pub(super) fn current_r_sources(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    expected: &[FinalRSource],
) -> Result<(), StorageError> {
    let ids = expected
        .iter()
        .map(|source| &source.fact.id)
        .collect::<Vec<_>>();
    let ids_json =
        serde_json::to_string(&ids).map_err(|error| StorageError::Schema(error.to_string()))?;
    let mut statement = tx.prepare(
        "WITH wanted AS (SELECT CAST(key AS INTEGER) AS position,value AS id FROM json_each(?2))
         SELECT wanted.position,n.id,n.payload_revision,n.turn_id,
            json_extract(n.payload_json,'$.group_id'),
            EXISTS(SELECT 1 FROM receipt q WHERE q.target_thread_id=?1 AND q.message_node_id=n.id AND q.state IN ('accepted','held')),
            CASE WHEN json_extract(n.payload_json,'$.group_id') IS NULL THEN 1 ELSE EXISTS(
              SELECT 1 FROM tool_group g WHERE g.id=json_extract(n.payload_json,'$.group_id') AND g.state='completed'
              AND NOT EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND
                (c.state!='completed' OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))) END,
            json_type(n.payload_json,'$.summary_format') IS NOT NULL,
            COALESCE((SELECT json_group_array(json_object('id',a.id,'path',a.absolute_path,'kind',a.input_kind,'state',COALESCE(a.state,'not_selected'),'reason',a.reason))
              FROM (SELECT ar.id,ar.absolute_path,ar.input_kind,ar.created_at,se.state,se.reason FROM attachment_ref ar
                LEFT JOIN attachment_selection se ON se.attachment_id=ar.id AND se.thread_id=?1
                WHERE ar.source_node_id=n.id ORDER BY ar.created_at,ar.id) a),'[]'),
            n.payload_json,n.deleted_reason
         FROM wanted LEFT JOIN node n ON n.id=wanted.id ORDER BY wanted.position",
    )?;
    let rows = statement.query_map(rusqlite::params![thread, ids_json], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<bool>>(5)?,
            row.get::<_, Option<bool>>(6)?,
            row.get::<_, Option<bool>>(7)?,
            row.get::<_, Option<String>>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, Option<String>>(10)?,
        ))
    })?;
    let rows = rows.collect::<Result<Vec<_>, _>>()?;
    if rows.len() != expected.len() {
        return Err(rejection(
            RejectionReason::SourceChanged,
            "summary source identity changed before adoption",
        ));
    }
    for (row, expected) in rows.into_iter().zip(expected) {
        let (
            _position,
            id,
            revision,
            turn,
            group_id,
            protected,
            group_complete,
            has_summary_marker,
            attachments,
            body,
            deleted_reason,
        ) = row;
        let Some(id) = id else {
            return Err(rejection(
                RejectionReason::SourceChanged,
                "summary source is no longer available",
            ));
        };
        let (Some(revision), Some(body)) = (revision, body) else {
            return Err(rejection(
                RejectionReason::SourcePruned,
                "summary source was pruned before adoption",
            ));
        };
        if deleted_reason.is_some() {
            return Err(rejection(
                RejectionReason::SourcePruned,
                "summary source was pruned before adoption",
            ));
        }
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            rejection(
                RejectionReason::SourceChanged,
                "summary source payload changed before adoption",
            )
        })?;
        let hash = super::key(&(revision, payload));
        let attachments: Vec<Value> = serde_json::from_str(attachments.as_deref().unwrap_or("[]"))
            .map_err(|error| StorageError::Schema(error.to_string()))?;
        if id != expected.fact.id
            || revision != expected.fact.revision
            || hash != expected.hash
            || turn != expected.fact.turn
            || group_id != expected.fact.group_id
            || protected != Some(expected.fact.protected)
            || group_complete != Some(expected.fact.group_complete)
            || has_summary_marker != Some(expected.fact.has_summary_marker)
            || attachments != expected.fact.attachments
        {
            return Err(rejection(
                RejectionReason::SourceChanged,
                "summary source facts changed before adoption",
            ));
        }
    }
    Ok(())
}

pub(super) fn current_path_ids(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    markers: &[FinalRSource],
) -> Result<(i64, Vec<String>), StorageError> {
    let current_revision: Option<i64> = tx
        .query_row(
            "SELECT selection_revision FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [thread],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current_revision) = current_revision else {
        return Err(rejection(
            RejectionReason::ThreadUnavailable,
            "summary target Thread is unavailable",
        ));
    };
    let mut path = tx
        .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
        .query_map([thread], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for marker in markers {
        let Some(position) = path.iter().position(|id| id == &marker.fact.id) else {
            continue;
        };
        current_r_sources(tx, thread, std::slice::from_ref(marker))?;
        let body: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM node WHERE id=?1",
                [&marker.fact.id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let payload: Value = body
            .as_deref()
            .ok_or_else(|| rejection(RejectionReason::SourcePruned, "summary marker was pruned"))
            .and_then(|body| {
                serde_json::from_str(body).map_err(|_| {
                    rejection(RejectionReason::SourceChanged, "summary marker changed")
                })
            })?;
        let sources = payload
            .get("source_node_ids")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                rejection(
                    RejectionReason::SourceChanged,
                    "summary marker source list changed",
                )
            })?
            .iter()
            .map(|id| {
                id.as_str().map(str::to_owned).ok_or_else(|| {
                    rejection(
                        RejectionReason::SourceChanged,
                        "summary marker source list changed",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if sources.is_empty() {
            return Err(rejection(
                RejectionReason::SourceChanged,
                "summary marker source list is empty",
            ));
        }
        path.splice(position..=position, sources);
    }
    Ok((current_revision, path))
}

pub(super) fn restore_current_marker_tx(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    cancel: &CancellationToken,
    marker: &FinalRSource,
    replacement_sources: &[FinalRSource],
    replacement_ids: &[String],
    remove_ids: &HashSet<String>,
) -> Result<(), StorageError> {
    if cancel.is_cancelled() {
        return Err(rejection(
            RejectionReason::Cancelled,
            "restore cancelled before adoption",
        ));
    }
    if replacement_ids.len() != replacement_sources.len()
        || replacement_ids
            .iter()
            .zip(replacement_sources)
            .any(|(id, source)| id != &source.fact.id)
        || replacement_ids.iter().collect::<HashSet<_>>().len() != replacement_ids.len()
    {
        return Err(StorageError::InvalidInput(
            "restore replacement IDs and source facts disagree".into(),
        ));
    }
    if remove_ids.iter().any(|id| !replacement_ids.contains(id)) {
        return Err(StorageError::InvalidInput(
            "restore removal set is not contained in the replacement".into(),
        ));
    }
    let (revision, mut path) = current_path_ids(tx, thread, &[])?;
    current_r_sources(tx, thread, std::slice::from_ref(marker))?;
    current_r_sources(tx, thread, replacement_sources)?;
    let marker_ids = [marker.fact.id.clone()];
    let start = locate_r(&path, &marker_ids)?;
    let end = start
        .checked_add(1)
        .ok_or_else(|| StorageError::InvalidInput("restore path position overflow".into()))?;
    let mut remove_positions = Vec::new();
    let mut previous_retained_position = None;
    for id in replacement_ids {
        let positions = path
            .iter()
            .enumerate()
            .filter_map(|(position, candidate)| (candidate == id).then_some(position))
            .collect::<Vec<_>>();
        if remove_ids.contains(id) {
            if positions.len() != 1
                || positions[0] <= start
                || previous_retained_position.is_some_and(|previous| positions[0] <= previous)
            {
                return Err(rejection(
                    RejectionReason::SourceChanged,
                    "restore retained anchors are no longer uniquely ordered after their marker",
                ));
            }
            previous_retained_position = Some(positions[0]);
            remove_positions.push(positions[0]);
        } else if !positions.is_empty() {
            return Err(rejection(
                RejectionReason::SourceChanged,
                "restore source is already selected outside its marker",
            ));
        }
    }
    remove_positions.sort_unstable();
    for position in remove_positions.into_iter().rev() {
        path.remove(position);
    }
    path.splice(start..end, replacement_ids.iter().cloned());
    if cancel.is_cancelled() {
        return Err(rejection(
            RejectionReason::Cancelled,
            "restore cancelled before path adoption",
        ));
    }
    history::select_path(tx, thread, revision as u64, &path, &timestamp())?;
    if cancel.is_cancelled() {
        return Err(rejection(
            RejectionReason::Cancelled,
            "restore cancelled during adoption",
        ));
    }
    Ok(())
}

pub(super) fn locate_r(path: &[String], ids: &[String]) -> Result<usize, StorageError> {
    if ids.is_empty() {
        return Err(rejection(
            RejectionReason::SourceChanged,
            "summary R is empty",
        ));
    }
    let mut matches = path
        .windows(ids.len())
        .enumerate()
        .filter_map(|(position, candidate)| (candidate == ids).then_some(position));
    let Some(position) = matches.next() else {
        return Err(rejection(
            RejectionReason::SourceChanged,
            "summary R is no longer contiguous on the selected path",
        ));
    };
    if matches.next().is_some() {
        return Err(rejection(
            RejectionReason::SourceChanged,
            "summary R has an ambiguous selected-path location",
        ));
    }
    Ok(position)
}

pub(super) struct FinalAdoption<'a> {
    pub(super) config: &'a Config,
    pub(super) cancel: &'a CancellationToken,
    pub(super) thread: &'a str,
    pub(super) sources: &'a [FinalRSource],
    pub(super) markers: &'a [FinalRSource],
    pub(super) payload: &'a Value,
    pub(super) input_key: &'a str,
    pub(super) format: &'a str,
}

#[derive(Clone, Copy)]
pub(super) enum FinalReasoningPolicy {
    PreserveCurrent,
    DropAll,
}

pub(super) fn adopt_current_r_tx(
    tx: &rusqlite::Transaction<'_>,
    request: FinalAdoption<'_>,
) -> Result<String, StorageError> {
    adopt_current_rs_tx(
        tx,
        std::slice::from_ref(&request),
        FinalReasoningPolicy::PreserveCurrent,
    )?
    .into_iter()
    .next()
    .ok_or_else(|| StorageError::Schema("single-R adoption returned no node".into()))
}

pub(super) fn adopt_current_rs_tx(
    tx: &rusqlite::Transaction<'_>,
    requests: &[FinalAdoption<'_>],
    reasoning_policy: FinalReasoningPolicy,
) -> Result<Vec<String>, StorageError> {
    let Some(first) = requests.first() else {
        return Err(StorageError::InvalidInput(
            "final adoption batch must not be empty".into(),
        ));
    };
    let thread = first.thread;
    let cancel = first.cancel;
    if requests
        .iter()
        .any(|request| request.thread != thread || !std::ptr::eq(request.cancel, cancel))
    {
        return Err(StorageError::InvalidInput(
            "final adoption batch must share one Thread and cancellation token".into(),
        ));
    }
    if cancel.is_cancelled() {
        return Err(rejection(
            RejectionReason::Cancelled,
            "compaction cancelled before adoption",
        ));
    }

    let mut markers = Vec::new();
    for marker in requests.iter().flat_map(|request| request.markers) {
        if let Some(existing) = markers
            .iter()
            .find(|item: &&FinalRSource| item.fact.id == marker.fact.id)
        {
            if existing.fact != marker.fact || existing.hash != marker.hash {
                return Err(rejection(
                    RejectionReason::SourceChanged,
                    "summary marker facts disagree within the adoption batch",
                ));
            }
        } else {
            markers.push(marker.clone());
        }
    }
    let (selection_revision, mut path) = current_path_ids(tx, thread, &markers)?;

    let mut intervals = Vec::with_capacity(requests.len());
    for (index, request) in requests.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(rejection(
                RejectionReason::Cancelled,
                "compaction cancelled while validating adoption batch",
            ));
        }
        current_r_sources(tx, thread, request.sources)?;
        let ids = request
            .sources
            .iter()
            .map(|source| source.fact.id.clone())
            .collect::<Vec<_>>();
        let start = locate_r(&path, &ids)?;
        let end = start
            .checked_add(ids.len())
            .ok_or_else(|| StorageError::InvalidInput("summary R path length overflow".into()))?;
        if super::gc_capture::native_tool_span_splits(
            tx,
            thread,
            i64::try_from(start).map_err(|_| {
                StorageError::InvalidInput("summary R path position overflow".into())
            })?,
            i64::try_from(end).map_err(|_| {
                StorageError::InvalidInput("summary R path position overflow".into())
            })?,
        )? {
            return Err(rejection(
                RejectionReason::SourceChanged,
                "summary R splits a tool call/result group",
            ));
        }
        intervals.push((start, end, index));
    }
    intervals.sort_unstable_by_key(|(start, _, _)| *start);
    if intervals.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(rejection(
            RejectionReason::SourceChanged,
            "final adoption intervals overlap on the current path",
        ));
    }

    let mut nodes = Vec::with_capacity(requests.len());
    for request in requests {
        let selected_sources = request
            .sources
            .iter()
            .map(|source| super::ReplacementSource {
                id: &source.fact.id,
                revision: source.fact.revision,
                hash: &source.hash,
            })
            .collect::<Vec<_>>();
        nodes.push(super::store_replacement_material_tx(
            tx,
            request.config,
            super::ReplacementMaterial {
                thread,
                input_key: request.input_key,
                format: request.format,
                sources: &selected_sources,
                payload: request.payload,
            },
        )?);
    }
    for (start, end, request_index) in intervals.iter().rev() {
        path.splice(*start..*end, [nodes[*request_index].clone()]);
    }

    let (projected_epoch, kept_reasoning_node_ids) =
        history::reasoning_projection_for_path(tx, thread, &path)?;
    let final_epoch = match reasoning_policy {
        FinalReasoningPolicy::PreserveCurrent => projected_epoch,
        FinalReasoningPolicy::DropAll => projected_epoch
            .checked_add(1)
            .ok_or_else(|| StorageError::InvalidInput("reasoning epoch overflow".into()))?,
    };
    for (request, node) in requests.iter().zip(&nodes) {
        let mut final_payload = request.payload.clone();
        if final_payload.get("summary_scope").is_some()
            || final_payload.get("reasoning_epoch").is_some()
        {
            final_payload["reasoning_epoch"] = json!(projected_epoch);
            if let Some(item) = final_payload.get_mut("item") {
                item["reasoning_epoch"] = json!(projected_epoch);
            }
            if matches!(reasoning_policy, FinalReasoningPolicy::DropAll)
                || kept_reasoning_node_ids.is_empty()
            {
                if let Some(object) = final_payload.as_object_mut() {
                    object.remove("kept_reasoning_node_ids");
                }
            } else {
                final_payload["kept_reasoning_node_ids"] = json!(kept_reasoning_node_ids);
            }
            let body = final_payload.to_string();
            tx.execute(
                "UPDATE node SET payload_json=?2,payload_bytes=?3,body_hash=?4 WHERE id=?1",
                rusqlite::params![node, body, body.len() as i64, super::key(&final_payload)],
            )?;
        }
    }
    if cancel.is_cancelled() {
        return Err(rejection(
            RejectionReason::Cancelled,
            "compaction cancelled before path adoption",
        ));
    }
    history::select_path(tx, thread, selection_revision as u64, &path, &timestamp())?;
    let actual_epoch: i64 = tx.query_row(
        "SELECT reasoning_epoch FROM thread WHERE id=?1",
        [thread],
        |row| row.get(0),
    )?;
    if actual_epoch != projected_epoch {
        return Err(StorageError::Schema(
            "final compaction reasoning projection differs from selected-path adoption".into(),
        ));
    }
    let actual_keep = history::reasoning_keep(tx, thread, actual_epoch)?;
    if kept_reasoning_node_ids
        .iter()
        .any(|id| !actual_keep.contains(id))
    {
        return Err(StorageError::Schema(
            "final compaction reasoning keep differs from selected-path adoption".into(),
        ));
    }
    if matches!(reasoning_policy, FinalReasoningPolicy::DropAll) {
        let changed = tx.execute(
            "UPDATE thread SET reasoning_epoch=?2 WHERE id=?1 AND reasoning_epoch=?3",
            rusqlite::params![thread, final_epoch, projected_epoch],
        )?;
        if changed != 1 {
            return Err(StorageError::Schema(
                "current reasoning epoch changed inside the final adoption transaction".into(),
            ));
        }
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [history::reasoning_keep_key(thread)],
        )?;
    }
    if cancel.is_cancelled() {
        return Err(rejection(
            RejectionReason::Cancelled,
            "compaction cancelled during adoption",
        ));
    }
    Ok(nodes)
}

fn manual_r_sources(
    snapshot: &Snapshot,
    start: usize,
    end: usize,
    expected_sources: &[Source],
) -> Result<Vec<FinalRSource>, StorageError> {
    let groups = snapshot.groups.get(start..end).ok_or_else(|| {
        StorageError::InvalidInput("manual summary R is outside its captured groups".into())
    })?;
    let mut actual = Vec::new();
    for group in groups {
        for source in &group.sources {
            actual.push(final_r_source(
                source,
                group.complete,
                snapshot.protected.contains(&source.id),
            ));
        }
    }
    if actual.len() != expected_sources.len()
        || actual
            .iter()
            .zip(expected_sources)
            .any(|(actual, expected)| actual.fact.id != expected.id || actual.hash != expected.hash)
    {
        return Err(rejection(
            RejectionReason::SourceChanged,
            "manual summary R differs from its generated source list",
        ));
    }
    Ok(actual)
}

pub(super) struct SummaryAdoption {
    pub(super) automatic: Option<AutomaticStage>,
    pub(super) removed_characters: u64,
    pub(super) snapshot: Snapshot,
    pub(super) sources: Vec<Source>,
    pub(super) payload: Value,
    pub(super) input_key: String,
    pub(super) start: usize,
    pub(super) count: usize,
    pub(super) result: CompactResult,
}
impl SummaryAdoption {
    pub(super) fn adopt_tx(
        mut self,
        tx: &rusqlite::Transaction<'_>,
        config: &Config,
        cancel: &CancellationToken,
    ) -> Result<CompactResult, StorageError> {
        let expected = manual_r_sources(&self.snapshot, self.start, self.count, &self.sources)?;
        let markers = self
            .snapshot
            .extra_sources
            .iter()
            .map(|source| {
                final_r_source(source, true, self.snapshot.protected.contains(&source.id))
            })
            .collect::<Vec<_>>();
        let node = adopt_current_r_tx(
            tx,
            FinalAdoption {
                config,
                cancel,
                thread: &self.snapshot.thread,
                sources: &expected,
                markers: &markers,
                payload: &self.payload,
                input_key: &self.input_key,
                format: super::FORMAT,
            },
        )?;
        if self.automatic.is_none() {
            let mut state = super::a_state::scope_tx(
                tx,
                &self.snapshot.thread,
                &self.snapshot.profile,
                &self.snapshot.model,
            )?;
            let expected = state.token();
            state.finish_boundary(expected, &node)?;
            super::a_state::save(tx, &self.snapshot.thread, &state)?;
        }
        self.result.summary_node_id = Some(node);
        if let Some(stage) = &self.automatic {
            super::attempt::record_tx(
                tx,
                &self.snapshot.thread,
                stage,
                &self.result,
                &super::attempt::StageMeasure {
                    removed_characters: Some(self.removed_characters),
                },
            )?;
        }
        if cancel.is_cancelled() {
            return Err(rejection(
                RejectionReason::Cancelled,
                "summary cancelled before transaction commit",
            ));
        }
        Ok(self.result)
    }
}

/// Native GC adoption over only the selected captured R. The caller runs this
/// inside the coordinator's strict original-token FINISH transaction.
pub(super) fn adopt_gc_result_tx(
    tx: &rusqlite::Transaction<'_>,
    config: &Config,
    captured: &super::gc_capture::CapturedSummaryInput,
    generated: &single_request::Dispatched,
    usage: &SummaryUsage,
    cancel: &CancellationToken,
) -> Result<String, StorageError> {
    let selected = generated.selected.clone();
    if selected.is_empty()
        || selected.end > captured.groups.len()
        || generated.response.text.trim().is_empty()
    {
        return Err(StorageError::InvalidInput(
            "GC summary has empty or invalid selected R".into(),
        ));
    }
    let selected_groups = &captured.groups[selected.clone()];
    let stamps = selected_groups
        .iter()
        .flat_map(|group| group.stamps.iter())
        .collect::<Vec<_>>();
    let originals = stamps
        .iter()
        .map(|stamp| stamp.id.clone())
        .collect::<Vec<_>>();
    let source_characters = selected_groups
        .iter()
        .try_fold(0u64, |sum, group| {
            sum.checked_add(u64::try_from(group.projection_weight).ok()?)
        })
        .ok_or_else(|| StorageError::InvalidInput("GC source projection overflow".into()))?;
    let text = if captured.active_turn {
        super::single_request::active_turn_summary_text(
            &originals,
            captured.active_user_messages.len(),
            false,
            &generated.response.text,
            None,
        )
    } else {
        generated.response.text.clone()
    };
    let identities = stamps
        .iter()
        .map(|stamp| super::SummarySourceIdentity {
            id: &stamp.id,
            revision: stamp.revision,
            hash: &stamp.hash,
            attachments: &stamp.attachments,
            marker_adapter: stamp.marker_adapter,
        })
        .collect::<Vec<_>>();
    let artifact = super::summary_artifact(super::SummaryArtifactInput {
        profile: &captured.profile,
        model: &captured.model,
        effort: captured.effort.as_deref(),
        epoch: captured.epoch,
        sources: &identities,
        source_ids: &originals,
        text: &text,
        usage,
        output_limit_exceeded: generated.size.exceeded_output_limit,
        scope: captured.active_turn.then_some("active_turn_middle"),
        kept_reasoning_ids: &[],
        reasoning_epoch: None,
    });
    if artifact.projection_characters >= source_characters {
        return Err(StorageError::InvalidInput(
            "GC summary does not shrink selected source projection".into(),
        ));
    }
    let expected = stamps
        .iter()
        .map(|stamp| FinalRSource {
            fact: stamp.source.clone(),
            hash: stamp.hash.clone(),
        })
        .collect::<Vec<_>>();
    adopt_current_r_tx(
        tx,
        FinalAdoption {
            config,
            cancel,
            thread: &captured.thread,
            sources: &expected,
            markers: &[],
            payload: &artifact.payload,
            input_key: &artifact.input_key,
            format: super::FORMAT,
        },
    )
}

#[cfg(test)]
mod final_r_adoption_tests {
    use super::*;
    use crate::compaction::discard::{Outcome, RejectionReason};
    use serde_json::json;

    async fn fixture() -> (tempfile::TempDir, crate::storage::DbWorker) {
        super::super::tests::database().await
    }

    async fn seed(db: &crate::storage::DbWorker) {
        db.call(|db| {
            let tx = db.transaction()?;
            for id in ["prefix", "r1", "r2", "suffix"] {
                history::append_node(
                    &tx,
                    "t",
                    id,
                    None,
                    "message",
                    &json!({"role":"user","content":id}),
                    "now",
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    }

    fn source(id: &str, complete: bool, protected: bool) -> FinalRSource {
        let payload = json!({"role":"user","content":id});
        let revision = 0;
        FinalRSource {
            fact: super::super::gc_capture::GcSourceFacts {
                ordinal: 0,
                id: id.into(),
                revision,
                turn: None,
                group_id: None,
                protected,
                group_complete: complete,
                has_summary_marker: false,
                attachments: Vec::new(),
            },
            hash: super::key(&(revision, payload)),
        }
    }

    fn grouped_source(id: &str, group: &str) -> FinalRSource {
        let payload = json!({"group_id":group,"output":[]});
        let revision = 0;
        FinalRSource {
            fact: super::super::gc_capture::GcSourceFacts {
                ordinal: 0,
                id: id.into(),
                revision,
                turn: Some("u".into()),
                group_id: Some(group.into()),
                protected: false,
                group_complete: true,
                has_summary_marker: false,
                attachments: Vec::new(),
            },
            hash: super::key(&(revision, payload)),
        }
    }

    fn summary_payload() -> Value {
        json!({"item":{"role":"developer","content":[{"type":"input_text","text":"summary"}]}})
    }

    #[tokio::test]
    async fn multi_spill_uses_one_current_epoch_projection_for_every_replacement() {
        let (_root, db) = fixture().await;
        seed(&db).await;
        db.call(|db| {
            let tx = db.transaction()?;
            tx.execute(
                "INSERT INTO node(id,kind,created_at,payload_json) VALUES('between','message','now','{}')",
                [],
            )?;
            let revision: i64 = tx.query_row(
                "SELECT selection_revision FROM thread WHERE id='t'",
                [],
                |row| row.get(0),
            )?;
            history::select_path(
                &tx,
                "t",
                revision as u64,
                &["prefix".into(), "r1".into(), "between".into(), "r2".into(), "suffix".into()],
                &timestamp(),
            )?;
            tx.execute("UPDATE thread SET reasoning_epoch=8 WHERE id='t'", [])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let first_sources = [source("r1", true, false)];
        let second_sources = [source("r2", true, false)];
        let first_payload = json!({"spill_tools":[],"output":[],"reasoning_epoch":1,"spill_version":1,"source_node_ids":["r1"]});
        let second_payload = json!({"spill_tools":[],"output":[],"reasoning_epoch":1,"spill_version":1,"source_node_ids":["r2"]});
        db.call(move |db| {
            let tx = db.transaction()?;
            let config = Config::default();
            let cancel = CancellationToken::new();
            let requests = [
                FinalAdoption {
                    config: &config,
                    cancel: &cancel,
                    thread: "t",
                    sources: &first_sources,
                    markers: &[],
                    payload: &first_payload,
                    input_key: "spill-batch-one",
                    format: "spill-v1",
                },
                FinalAdoption {
                    config: &config,
                    cancel: &cancel,
                    thread: "t",
                    sources: &second_sources,
                    markers: &[],
                    payload: &second_payload,
                    input_key: "spill-batch-two",
                    format: "spill-v1",
                },
            ];
            let nodes = adopt_current_rs_tx(&tx, &requests, FinalReasoningPolicy::DropAll)?;
            assert_eq!(nodes.len(), 2);
            let epoch: i64 = tx.query_row(
                "SELECT reasoning_epoch FROM thread WHERE id='t'",
                [],
                |row| row.get(0),
            )?;
            assert!(epoch > 8, "drop-all advances the live projected epoch");
            for node in &nodes {
                let body: String =
                    tx.query_row("SELECT payload_json FROM node WHERE id=?1", [node], |row| {
                        row.get(0)
                    })?;
                let payload: Value = serde_json::from_str(&body)
                    .map_err(|error| StorageError::Schema(error.to_string()))?;
                let stored_epoch = payload["reasoning_epoch"].as_i64().unwrap();
                if let Some(previous) = payload_epoch {
                    assert_eq!(
                        stored_epoch, previous,
                        "all batch spills share one projection epoch"
                    );
                }
                payload_epoch = Some(stored_epoch);
                assert!(payload.get("kept_reasoning_node_ids").is_none());
            }
            let spill_projection_epoch = payload_epoch.expect("two spill payloads");
            assert_eq!(
                spill_projection_epoch + 1,
                epoch,
                "DROP_ALL advances from the live projection exactly once"
            );
            assert!(history::reasoning_keep(&tx, "t", epoch)?.is_empty());
            let path = tx
                .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(
                path,
                [
                    "prefix",
                    nodes[0].as_str(),
                    "between",
                    nodes[1].as_str(),
                    "suffix"
                ]
            );
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn spill_format_uses_final_r_and_preserves_unrelated_current_prefix_suffix() {
        let (_root, db) = fixture().await;
        seed(&db).await;
        db.call(|db| {
            let tx = db.transaction()?;
            tx.execute(
                "INSERT INTO node(id,kind,created_at,payload_json) VALUES('new-prefix','message','now','{}')",
                [],
            )?;
            let revision: i64 = tx.query_row(
                "SELECT selection_revision FROM thread WHERE id='t'",
                [],
                |row| row.get(0),
            )?;
            history::select_path(
                &tx,
                "t",
                revision as u64,
                &["new-prefix".into(), "prefix".into(), "r1".into(), "r2".into(), "suffix".into()],
                &timestamp(),
            )?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let payload = json!({"spill_tools":[],"output":[],"reasoning_epoch":0,"spill_version":1,"source_node_ids":["r1","r2"]});
        db.call(move |db| {
            let tx = db.transaction()?;
            let config = Config::default();
            let cancel = CancellationToken::new();
            let sources = [source("r1", true, false), source("r2", true, false)];
            let node = adopt_current_r_tx(
                &tx,
                FinalAdoption {
                    config: &config,
                    cancel: &cancel,
                    thread: "t",
                    sources: &sources,
                    markers: &[],
                    payload: &payload,
                    input_key: "spill-final-r",
                    format: "spill-v1",
                },
            )?;
            let kind: String =
                tx.query_row("SELECT kind FROM node WHERE id=?1", [&node], |row| {
                    row.get(0)
                })?;
            assert_eq!(kind, "spill");
            let path = tx
                .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(path, ["new-prefix", "prefix", node.as_str(), "suffix"]);
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn restore_splices_current_marker_without_overwriting_external_prefix_suffix() {
        let (_root, db) = fixture().await;
        seed(&db).await;
        let marker_payload =
            json!({"summary_format":"context-trim-v1","source_node_ids":["r1","r2"]});
        let marker_payload_for_insert = marker_payload.clone();
        db.call(move |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('marker','summary','now',?1)", [marker_payload_for_insert.to_string()])?;
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('new-prefix','message','now','{}')", [])?;
            let revision: i64 = tx.query_row("SELECT selection_revision FROM thread WHERE id='t'", [], |row| row.get(0))?;
            history::select_path(&tx, "t", revision as u64, &["new-prefix".into(),"prefix".into(),"marker".into(),"suffix".into()], &timestamp())?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let marker = FinalRSource {
            fact: super::super::gc_capture::GcSourceFacts {
                ordinal: 0,
                id: "marker".into(),
                revision: 0,
                turn: None,
                group_id: None,
                protected: false,
                group_complete: true,
                has_summary_marker: true,
                attachments: Vec::new(),
            },
            hash: super::key(&(0, marker_payload)),
        };
        let replacements = [source("r1", true, false), source("r2", true, false)];
        let replacement_ids = vec!["r1".into(), "r2".into()];
        db.call(move |db| {
            let tx = db.transaction()?;
            restore_current_marker_tx(
                &tx,
                "t",
                &CancellationToken::new(),
                &marker,
                &replacements,
                &replacement_ids,
                &HashSet::new(),
            )?;
            let path = tx
                .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(path, ["new-prefix", "prefix", "r1", "r2", "suffix"]);
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn restore_rejects_reordered_retained_anchors_without_changing_current_path() {
        let (_root, db) = fixture().await;
        seed(&db).await;
        let marker_payload =
            json!({"summary_format":"context-trim-v1","source_node_ids":["r1","r2"]});
        let marker_payload_for_insert = marker_payload.clone();
        db.call(move |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('marker','summary','now',?1)", [marker_payload_for_insert.to_string()])?;
            let revision: i64 = tx.query_row("SELECT selection_revision FROM thread WHERE id='t'", [], |row| row.get(0))?;
            history::select_path(&tx, "t", revision as u64, &["prefix".into(),"marker".into(),"r1".into(),"r2".into(),"suffix".into()], &timestamp())?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        db.call(|db| {
            let tx = db.transaction()?;
            tx.execute("DELETE FROM thread_path WHERE thread_id='t'", [])?;
            for (ordinal, id) in ["prefix", "marker", "r2", "r1", "suffix"]
                .iter()
                .enumerate()
            {
                tx.execute(
                    "INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('t',?1,?2)",
                    rusqlite::params![ordinal as i64, id],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
        let marker = FinalRSource {
            fact: super::super::gc_capture::GcSourceFacts {
                ordinal: 0,
                id: "marker".into(),
                revision: 0,
                turn: None,
                group_id: None,
                protected: false,
                group_complete: true,
                has_summary_marker: true,
                attachments: Vec::new(),
            },
            hash: super::key(&(0, marker_payload)),
        };
        let replacements = [source("r1", true, false), source("r2", true, false)];
        let replacement_ids = vec!["r1".into(), "r2".into()];
        let remove_ids = ["r1".to_owned(), "r2".to_owned()].into_iter().collect();
        db.call(move |db| {
            let tx = db.transaction()?;
            let error = restore_current_marker_tx(
                &tx,
                "t",
                &CancellationToken::new(),
                &marker,
                &replacements,
                &replacement_ids,
                &remove_ids,
            )
            .unwrap_err()
            .into_api_error();
            assert_eq!(
                crate::compaction::discard::cause(&error),
                super::super::discard::Outcome::Rejected {
                    reason: RejectionReason::SourceChanged,
                }
            );
            let path = tx
                .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(path, ["prefix", "marker", "r2", "r1", "suffix"]);
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn restore_rejects_changed_original_source_without_changing_current_path() {
        let (_root, db) = fixture().await;
        seed(&db).await;
        let marker_payload =
            json!({"summary_format":"context-trim-v1","source_node_ids":["r1","r2"]});
        let marker_payload_for_insert = marker_payload.clone();
        db.call(move |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('marker','summary','now',?1)", [marker_payload_for_insert.to_string()])?;
            let revision: i64 = tx.query_row("SELECT selection_revision FROM thread WHERE id='t'", [], |row| row.get(0))?;
            history::select_path(&tx, "t", revision as u64, &["prefix".into(),"marker".into(),"suffix".into()], &timestamp())?;
            tx.execute("UPDATE node SET payload_revision=payload_revision+1,payload_json='{}' WHERE id='r1'", [])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let marker = FinalRSource {
            fact: super::super::gc_capture::GcSourceFacts {
                ordinal: 0,
                id: "marker".into(),
                revision: 0,
                turn: None,
                group_id: None,
                protected: false,
                group_complete: true,
                has_summary_marker: true,
                attachments: Vec::new(),
            },
            hash: super::key(&(0, marker_payload)),
        };
        let replacements = [source("r1", true, false), source("r2", true, false)];
        let replacement_ids = vec!["r1".into(), "r2".into()];
        db.call(move |db| {
            let tx = db.transaction()?;
            let error = restore_current_marker_tx(
                &tx,
                "t",
                &CancellationToken::new(),
                &marker,
                &replacements,
                &replacement_ids,
                &HashSet::new(),
            )
            .unwrap_err()
            .into_api_error();
            assert_eq!(
                crate::compaction::discard::cause(&error),
                super::super::discard::Outcome::Rejected {
                    reason: RejectionReason::SourceChanged,
                }
            );
            let path = tx
                .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(path, ["prefix", "marker", "suffix"]);
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn final_r_splices_current_path_between_new_prefix_and_suffix_after_outside_changes() {
        let (_root, db) = fixture().await;
        seed(&db).await;
        db.call(|db| {
            let tx = db.transaction()?;
            tx.execute(
                "INSERT INTO node(id,kind,created_at,payload_json) VALUES('new-prefix','message','now','{}')",
                [],
            )?;
            history::select_path(
                &tx,
                "t",
                history::selection_revision(&tx, "t", history::ThreadScope::Live)? as u64,
                &["new-prefix", "prefix", "r1", "r2", "suffix"].map(String::from),
                "now",
            )?;
            tx.execute("UPDATE thread SET profile_revision=profile_revision+1 WHERE id='t'", [])?;
            tx.execute("UPDATE thread SET reasoning_epoch=reasoning_epoch+1 WHERE id='t'", [])?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
        db.call(|db| {
            let tx = db.transaction()?;
            let config = Config::default();
            let cancel = CancellationToken::new();
            let sources = [source("r1", true, false), source("r2", true, false)];
            let payload = summary_payload();
            let node = adopt_current_r_tx(
                &tx,
                FinalAdoption {
                    config: &config,
                    cancel: &cancel,
                    thread: "t",
                    sources: &sources,
                    markers: &[],
                    payload: &payload,
                    input_key: "final-r-positive",
                    format: super::FORMAT,
                },
            )?;
            let path = tx
                .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(path, ["new-prefix", "prefix", node.as_str(), "suffix"]);
            assert_eq!(
                tx.query_row(
                    "SELECT group_concat(source_node_id,',') FROM replacement_source JOIN replacement ON replacement.id=replacement_source.replacement_id WHERE replacement.output_node_id=?1 ORDER BY ordinal",
                    [&node],
                    |row| row.get::<_, String>(0),
                )?,
                "r1,r2"
            );
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn final_r_rejects_partial_tool_group() {
        let (_root, db) = fixture().await;
        db.call(|db| {
            let tx = db.transaction()?;
            tx.execute_batch(
                "INSERT INTO run(id,thread_id,state,created_at) VALUES('run','t','completed','now');
                 INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','run','completed','now');
                 INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)
                   VALUES('attempt','u',0,0,'fixture','fixture',0,'completed','{}','now');
                 INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at) VALUES('g','attempt','u','completed','now');
                 INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)
                   VALUES('g','call',0,'shell','{}','completed');",
            )?;
            history::append_node(
                &tx,
                "t",
                "call-node",
                Some("u"),
                "assistant_response",
                &json!({"group_id":"g","output":[{"type":"function_call","call_id":"call","name":"shell","arguments":"{}"}]}),
                "now",
            )?;
            history::append_node(
                &tx,
                "t",
                "result-node",
                Some("u"),
                "tool_result",
                &json!({"group_id":"g","item":{"type":"function_call_output","call_id":"call","output":"done"}}),
                "now",
            )?;
            tx.execute(
                "INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)
                 VALUES('attempt',0,'function_call','completed',?1,'call-node')",
                [json!({"call_id":"call"}).to_string()],
            )?;
            tx.execute(
                "INSERT INTO tool_result(group_id,call_id,node_id,status) VALUES('g','call','result-node','completed')",
                [],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
        db.call(|db| {
            let tx = db.transaction()?;
            let result = adopt_current_r_tx(
                &tx,
                FinalAdoption {
                    config: &Config::default(),
                    cancel: &CancellationToken::new(),
                    thread: "t",
                    sources: &[grouped_source("call-node", "g")],
                    markers: &[],
                    payload: &summary_payload(),
                    input_key: "final-r-partial-tool",
                    format: super::FORMAT,
                },
            );
            let error = result.unwrap_err().into_api_error();
            assert_eq!(
                crate::compaction::discard::cause(&error),
                Outcome::Rejected {
                    reason: RejectionReason::SourceChanged
                }
            );
            assert_eq!(
                tx.query_row("SELECT count(*) FROM replacement", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
        db.shutdown().await.unwrap();
    }

    async fn rejects(kind: &'static str) {
        let (_root, db) = fixture().await;
        seed(&db).await;
        db.call(move |db| {
            let tx = db.transaction()?;
            match kind {
                "mutation" => {
                    tx.execute(
                        "UPDATE node SET payload_revision=payload_revision+1,payload_json='{}' WHERE id='r1'",
                        [],
                    )?;
                }
                "noncontiguous" => history::select_path(
                    &tx,
                    "t",
                    history::selection_revision(&tx, "t", history::ThreadScope::Live)? as u64,
                    &["prefix", "r1", "suffix"].map(String::from),
                    "now",
                )?,
                "missing" => {
                    tx.execute("UPDATE thread SET deleted_at='now' WHERE id='t'", [])?;
                }
                "pruned" => {
                    tx.execute(
                        "UPDATE node SET payload_json=NULL,deleted_reason='pruned' WHERE id='r1'",
                        [],
                    )?;
                }
                "protected" => {
                    tx.execute(
                        "INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)
                         VALUES('q','op','{}','t','r1','accepted','now')",
                        [],
                    )?;
                }
                "cancel" => {}
                _ => unreachable!(),
            }
            let before = tx
                .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let cancel = CancellationToken::new();
            if kind == "cancel" {
                cancel.cancel();
            }
            let config = Config::default();
            let payload = summary_payload();
            let result = adopt_current_r_tx(
                &tx,
                FinalAdoption {
                    config: &config,
                    cancel: &cancel,
                    thread: "t",
                    sources: &[source("r1", true, false), source("r2", true, false)],
                    markers: &[],
                    payload: &payload,
                    input_key: "final-r-negative",
                    format: super::FORMAT,
                },
            );
            let expected = match kind {
                "missing" => RejectionReason::ThreadUnavailable,
                "pruned" => RejectionReason::SourcePruned,
                "cancel" => RejectionReason::Cancelled,
                _ => RejectionReason::SourceChanged,
            };
            let error = result.unwrap_err().into_api_error();
            assert_eq!(crate::compaction::discard::cause(&error), Outcome::Rejected { reason: expected });
            assert_eq!(tx.query_row("SELECT count(*) FROM replacement", [], |row| row.get::<_, i64>(0))?, 0);
            let after = tx
                .prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(before, after);
            Ok(())
        })
        .await
        .unwrap();
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn changed_r_rejects_atomically() {
        rejects("mutation").await;
    }
    #[tokio::test]
    async fn noncontiguous_r_rejects_atomically() {
        rejects("noncontiguous").await;
    }
    #[tokio::test]
    async fn pruned_r_rejects_atomically() {
        rejects("pruned").await;
    }
    #[tokio::test]
    async fn protected_r_rejects_atomically() {
        rejects("protected").await;
    }
    #[tokio::test]
    async fn unavailable_thread_and_cancellation_reject_atomically() {
        rejects("missing").await;
        rejects("cancel").await;
    }
}
