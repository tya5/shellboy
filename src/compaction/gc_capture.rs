//! Bounded selected-range source capture for the GC-only summary path.
use super::{RenderSourceView, invalid, key, render_into};
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{collections::HashMap, ops::Range};

#[derive(Clone, Debug)]
pub(crate) struct CapturedSourceStamp {
    pub(crate) ordinal: i64,
    pub(crate) id: String,
    pub(crate) turn: Option<String>,
    pub(crate) revision: i64,
    pub(crate) hash: String,
    pub(crate) marker_adapter: bool,
    pub(crate) group_id: Option<String>,
    pub(crate) protected: bool,
    pub(crate) attachments: Vec<Value>,
}

#[derive(Clone, Debug)]
pub(crate) struct CapturedGroup {
    pub(crate) stamps: Vec<CapturedSourceStamp>,
    pub(crate) text: String,
    pub(crate) projection_weight: u128,
    pub(crate) has_unsummarized: bool,
    pub(crate) complete: bool,
    pub(crate) tool: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct GcPlannedSource {
    pub(crate) ordinal: i64,
    pub(crate) id: String,
    pub(crate) revision: i64,
    pub(crate) turn: Option<String>,
    pub(crate) group_id: Option<String>,
    pub(crate) protected: bool,
    pub(crate) group_complete: bool,
    pub(crate) has_summary_marker: bool,
    pub(crate) current_turn_user: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct GcRangePlan {
    pub(crate) range: Range<usize>,
    pub(crate) sources: Vec<GcPlannedSource>,
    pub(crate) selection_revision: i64,
    pub(crate) profile_revision: i64,
    pub(crate) nonappend_generation: i64,
    pub(crate) epoch: i64,
    pub(crate) profile: String,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) stage: super::attempt::GcStage,
    pub(crate) active_tail_range: Option<Range<usize>>,
    pub(crate) active_turn: bool,
    pub(crate) spill_epoch_advanced: bool,
    pub(crate) drop_reasoning: bool,
    pub(crate) active_user_messages: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CapturedSummaryInput {
    pub(crate) thread: String,
    pub(crate) selection_revision: i64,
    pub(crate) profile_revision: i64,
    pub(crate) nonappend_generation: i64,
    pub(crate) epoch: i64,
    pub(crate) source_characters: u64,
    pub(crate) profile: String,
    pub(crate) model: String,
    pub(crate) thread_summary_max_chars: Option<u64>,
    pub(crate) effort: Option<String>,
    pub(crate) origin_turn: Option<String>,
    pub(crate) origin_run: Option<String>,
    pub(crate) origin_generation: Option<i64>,
    pub(crate) request_turn_id: Option<String>,
    pub(crate) request_generation: Option<i64>,
    pub(crate) groups: Vec<CapturedGroup>,
    pub(crate) active_tail_group: Option<usize>,
    pub(crate) active_turn: bool,
    pub(crate) spill_epoch_advanced: bool,
    pub(crate) drop_reasoning: bool,
    pub(crate) active_user_messages: Vec<String>,
}

#[derive(Clone)]
struct RowPlan {
    ordinal: i64,
    id: String,
    turn: Option<String>,
    revision: i64,
    group_id: Option<String>,
    complete: bool,
    protected: bool,
    summary: bool,
}

impl CapturedSourceStamp {
    pub(crate) fn fingerprint(&self) -> (&str, i64, i64, Option<&str>, &str, bool) {
        (
            &self.id,
            self.ordinal,
            self.revision,
            self.turn.as_deref(),
            &self.hash,
            self.protected,
        )
    }
}

/// Plan only the ordinary-new prefix from bounded path metadata. The caller
/// consumes this plan in the same capture transaction; no node payload Values
/// or Run/Turn lifetime fences enter the plan.
fn plan_gc_prefix_tx(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    stage: &super::attempt::GcStage,
) -> Result<Option<GcRangePlan>, StorageError> {
    let (selection,profile_revision,generation,epoch,profile,model,effort,current_turn,latest_turn,spill_committed):
        (i64,i64,i64,i64,String,String,Option<String>,Option<String>,Option<String>,bool)=tx.query_row(
        "SELECT selection_revision,profile_revision,selection_nonappend_generation,reasoning_epoch,profile_id,model_id,effort,(SELECT id FROM turn WHERE thread_id=?1 AND state IN ('preparing','running','waiting','recovery_wait') ORDER BY rowid DESC LIMIT 1),(SELECT id FROM turn WHERE thread_id=?1 AND state='completed' ORDER BY rowid DESC LIMIT 1),EXISTS(SELECT 1 FROM model_attempt a WHERE a.id=?2 AND json_extract(a.error_json,'$.automatic_trim_attempt.token')=?3 AND (json_extract(a.error_json,'$.automatic_trim_attempt.committed_stages.active_spill.reasoning_epoch')=(SELECT reasoning_epoch FROM thread WHERE id=?1) OR json_extract(a.error_json,'$.automatic_trim_attempt.committed_stages.closed_spill.reasoning_epoch')=(SELECT reasoning_epoch FROM thread WHERE id=?1))) FROM thread WHERE id=?1 AND deleted_at IS NULL",
        params![thread,stage.attempt_id,stage.token],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?)),
    )?;
    let spill_epoch_advanced =
        stage.reasoning_epoch.checked_add(1) == Some(epoch) && spill_committed;
    if stage.thread_id != thread
        || stage.profile_id != profile
        || stage.model_id != model
        || stage.effort != effort
        || (stage.reasoning_epoch != epoch && !spill_epoch_advanced)
    {
        return Err(StorageError::InvalidInput(format!("GC range-plan context mismatch thread={} profile={} model={} effort={} stage_epoch={} thread_epoch={} committed_spill={}",stage.thread_id==thread,stage.profile_id==profile,stage.model_id==model,stage.effort==effort,stage.reasoning_epoch,epoch,spill_committed).into()));
    }
    let current_boundary = current_turn
        .as_deref()
        .map(|turn| super::resolve_turn_boundary(tx, thread, turn, super::TURN_BOUNDARY_IDS))
        .transpose()?
        .flatten();
    let latest_boundary = latest_turn
        .as_deref()
        .map(|turn| super::resolve_turn_boundary(tx, thread, turn, super::TURN_BOUNDARY_IDS))
        .transpose()?
        .flatten();

    // Stream scalar path metadata. Only merged group fences are retained for
    // the whole path; node identities are copied only for the resulting R.
    let mut statement = tx.prepare(
        "SELECT p.ordinal,n.id,n.turn_id,json_extract(n.payload_json,'$.group_id'),
            json_type(n.payload_json,'$.summary_format') IS NOT NULL,
            EXISTS(SELECT 1 FROM receipt q WHERE q.target_thread_id=?1 AND q.message_node_id=n.id AND q.state IN ('accepted','held')),
            CASE WHEN json_extract(n.payload_json,'$.group_id') IS NULL THEN p.ordinal ELSE max(p.ordinal) OVER (PARTITION BY json_extract(n.payload_json,'$.group_id')) END,
            CASE WHEN json_extract(n.payload_json,'$.group_id') IS NULL THEN 1 ELSE EXISTS(SELECT 1 FROM tool_group g WHERE g.id=json_extract(n.payload_json,'$.group_id') AND g.state='completed' AND NOT EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state!='completed' OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))) END
         FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 ORDER BY p.ordinal",
    )?;
    let mut rows = statement.query([thread])?;
    let mut groups: Vec<(Range<usize>, bool, bool, bool, bool)> = Vec::new();
    let mut next_ordinal = 0usize;
    let mut merger = SourceGroupRangeMerger::new();
    let mut group_tool = false;
    let mut group_complete = true;
    let mut group_protected = false;
    let mut group_all_summaries = true;
    let mut group_boundary = false;
    let mut latest_tool = None;
    let mut first_unsafe = None;
    let mut normal = usize::MAX;
    while let Some(row) = rows.next()? {
        let ordinal = usize::try_from(row.get::<_, i64>(0)?).map_err(|_| overflow())?;
        if ordinal != next_ordinal {
            return Err(StorageError::Schema(
                "thread_path ordinals are not contiguous".into(),
            ));
        }
        let id: String = row.get(1)?;
        let turn: Option<String> = row.get(2)?;
        let group: Option<String> = row.get(3)?;
        let summary: bool = row.get(4)?;
        let protected: bool = row.get(5)?;
        let last: i64 = row.get(6)?;
        let complete: bool = row.get(7)?;
        let last = usize::try_from(last).map_err(|_| overflow())?;
        group_tool |= group.is_some();
        group_complete &= complete;
        group_protected |= protected;
        group_all_summaries &= summary;
        group_boundary |= turn.as_deref().is_some_and(|turn| {
            Some(turn) == current_turn.as_deref() || Some(turn) == latest_turn.as_deref()
        }) || current_boundary.as_deref() == Some(id.as_str())
            || latest_boundary.as_deref() == Some(id.as_str());
        next_ordinal += 1;
        if let Some(span) = merger.push(ordinal, group.as_deref(), last) {
            let index = groups.len();
            let tool = group_tool;
            let safe_group = group_complete && !group_protected;
            groups.try_reserve(1).map_err(|_| overflow())?;
            groups.push((
                span,
                tool,
                group_complete,
                group_protected,
                group_all_summaries,
            ));
            if !safe_group {
                first_unsafe.get_or_insert(index);
            }
            if group_boundary {
                normal = normal.min(index);
            }
            if tool && group_complete {
                latest_tool = Some(index);
            }
            group_tool = false;
            group_complete = true;
            group_protected = false;
            group_all_summaries = true;
            group_boundary = false;
        }
    }
    drop(rows);
    drop(statement);
    if next_ordinal == 0 {
        return Ok(None);
    }
    // `safe` is the first unsafe group, or the group count if every group is safe.
    let safe = first_unsafe.unwrap_or(groups.len());
    let base_count = safe.min(normal).min(latest_tool.unwrap_or(groups.len()));
    if base_count == 0 || !groups[..base_count].iter().any(|group| !group.4) {
        return Ok(None);
    }
    let prefix_count = base_count;
    let end = groups[prefix_count - 1].0.end;
    let mut sources = Vec::new();
    sources.try_reserve_exact(end).map_err(|_| overflow())?;
    let mut selected=tx.prepare(
        "SELECT p.ordinal,n.id,n.payload_revision,n.turn_id,json_extract(n.payload_json,'$.group_id'),
            EXISTS(SELECT 1 FROM receipt q WHERE q.target_thread_id=?1 AND q.message_node_id=n.id AND q.state IN ('accepted','held')),
            CASE WHEN json_extract(n.payload_json,'$.group_id') IS NULL THEN 1 ELSE EXISTS(SELECT 1 FROM tool_group g WHERE g.id=json_extract(n.payload_json,'$.group_id') AND g.state='completed' AND NOT EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state!='completed' OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))) END,
            json_type(n.payload_json,'$.summary_format') IS NOT NULL,
            CASE WHEN n.payload_json IS NULL OR json_type(n.payload_json,'$.output') IS NOT NULL THEN 0 WHEN n.turn_id IS ?3 AND (json_extract(n.payload_json,'$.role')='user' OR json_extract(n.payload_json,'$.item.role')='user') THEN 1 ELSE 0 END
         FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal<?2 ORDER BY p.ordinal",
    )?;
    let selected_rows = selected.query_map(
        params![
            thread,
            i64::try_from(end).map_err(|_| overflow())?,
            current_turn
        ],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, bool>(7)?,
                row.get::<_, bool>(8)?,
            ))
        },
    )?;
    for row in selected_rows {
        let (
            ordinal,
            id,
            revision,
            turn,
            group_id,
            protected,
            group_complete,
            has_summary_marker,
            current_turn_user,
        ) = row?;
        sources.push(GcPlannedSource {
            ordinal,
            id,
            revision,
            turn,
            group_id,
            protected,
            group_complete,
            has_summary_marker,
            current_turn_user,
        });
    }
    if sources.len() != end {
        return Err(StorageError::Schema(
            "planned GC R changed during metadata selection".into(),
        ));
    }
    Ok(Some(GcRangePlan {
        range: 0..end,
        sources,
        selection_revision: selection,
        profile_revision,
        nonappend_generation: generation,
        epoch,
        profile,
        model,
        effort,
        stage: stage.clone(),
        active_tail_range: None,
        active_turn: false,
        spill_epoch_advanced,
        drop_reasoning: false,
        active_user_messages: Vec::new(),
    }))
}

fn plan_gc_active_turn_tx(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    stage: &super::attempt::GcStage,
    expected_turn: &str,
) -> Result<Option<GcRangePlan>, StorageError> {
    let (selection,profile_revision,generation,epoch,profile,model,effort,current_turn,latest_turn,spill_committed):(i64,i64,i64,i64,String,String,Option<String>,Option<String>,Option<String>,bool)=tx.query_row(
        "SELECT selection_revision,profile_revision,selection_nonappend_generation,reasoning_epoch,profile_id,model_id,effort,(SELECT id FROM turn WHERE thread_id=?1 AND state IN ('preparing','running','waiting','recovery_wait') ORDER BY rowid DESC LIMIT 1),(SELECT id FROM turn WHERE thread_id=?1 AND state='completed' ORDER BY rowid DESC LIMIT 1),EXISTS(SELECT 1 FROM model_attempt a WHERE a.id=?2 AND json_extract(a.error_json,'$.automatic_trim_attempt.token')=?3 AND (json_extract(a.error_json,'$.automatic_trim_attempt.committed_stages.active_spill.reasoning_epoch')=(SELECT reasoning_epoch FROM thread WHERE id=?1) OR json_extract(a.error_json,'$.automatic_trim_attempt.committed_stages.closed_spill.reasoning_epoch')=(SELECT reasoning_epoch FROM thread WHERE id=?1))) FROM thread WHERE id=?1 AND deleted_at IS NULL",
        params![thread,stage.attempt_id,stage.token],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))?;
    let Some(current_turn) = current_turn else {
        return Ok(None);
    };
    if current_turn != expected_turn {
        return Err(StorageError::InvalidInput(
            "active GC parent Turn differs from current Thread Turn".into(),
        ));
    }
    let spill_epoch_advanced =
        stage.reasoning_epoch.checked_add(1) == Some(epoch) && spill_committed;
    if stage.thread_id != thread
        || stage.profile_id != profile
        || stage.model_id != model
        || stage.effort != effort
        || (stage.reasoning_epoch != epoch && !spill_epoch_advanced)
    {
        return Err(StorageError::InvalidInput(format!("GC active plan context mismatch: thread={} profile={} model={} effort={} epoch={} committed_spill={}",stage.thread_id==thread,stage.profile_id==profile,stage.model_id==model,stage.effort==effort,stage.reasoning_epoch==epoch,spill_committed).into()));
    }
    let current_boundary =
        super::resolve_turn_boundary(tx, thread, &current_turn, super::TURN_BOUNDARY_IDS)?;
    let latest_boundary = latest_turn
        .as_deref()
        .map(|turn| super::resolve_turn_boundary(tx, thread, turn, super::TURN_BOUNDARY_IDS))
        .transpose()?
        .flatten();
    let mut statement=tx.prepare(
        "SELECT p.ordinal,n.id,n.turn_id,json_extract(n.payload_json,'$.group_id'),EXISTS(SELECT 1 FROM receipt q WHERE q.target_thread_id=?1 AND q.message_node_id=n.id AND q.state IN ('accepted','held')),CASE WHEN json_extract(n.payload_json,'$.group_id') IS NULL THEN p.ordinal ELSE max(p.ordinal) OVER (PARTITION BY json_extract(n.payload_json,'$.group_id')) END,CASE WHEN json_extract(n.payload_json,'$.group_id') IS NULL THEN 1 ELSE EXISTS(SELECT 1 FROM tool_group g WHERE g.id=json_extract(n.payload_json,'$.group_id') AND g.state='completed' AND NOT EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state!='completed' OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))) END,CASE WHEN n.payload_json IS NULL OR json_type(n.payload_json,'$.output') IS NOT NULL THEN 0 WHEN n.turn_id IS ?2 AND (json_extract(n.payload_json,'$.role')='user' OR json_extract(n.payload_json,'$.item.role')='user') THEN 1 ELSE 0 END,json_type(n.payload_json,'$.spill_version') IS NOT NULL,json_type(n.payload_json,'$.summary_scope') IS NOT NULL,json_type(n.payload_json,'$.context_trim') IS NOT NULL FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 ORDER BY p.ordinal")?;
    let mut rows = statement.query(params![thread, current_turn])?;
    let mut groups: Vec<(Range<usize>, bool, bool, bool, bool, bool, bool)> = Vec::new();
    let mut merger = SourceGroupRangeMerger::new();
    let mut ordinal_next = 0usize;
    let mut tool = false;
    let mut complete = true;
    let mut protected = false;
    let mut user = false;
    let mut spill = false;
    let mut marker = false;
    let mut context_trim = false;
    let mut request_group = None;
    let mut latest_tool = None;
    let mut boundary_group = None;
    while let Some(row) = rows.next()? {
        let ordinal = usize::try_from(row.get::<_, i64>(0)?).map_err(|_| overflow())?;
        if ordinal != ordinal_next {
            return Err(StorageError::Schema(
                "thread_path ordinals are not contiguous".into(),
            ));
        }
        let id: String = row.get(1)?;
        let turn: Option<String> = row.get(2)?;
        let group: Option<String> = row.get(3)?;
        let is_protected: bool = row.get(4)?;
        let last = usize::try_from(row.get::<_, i64>(5)?).map_err(|_| overflow())?;
        let group_complete: bool = row.get(6)?;
        let is_user: bool = row.get(7)?;
        let is_spill: bool = row.get(8)?;
        let is_marker: bool = row.get(9)?;
        let is_trim: bool = row.get(10)?;
        user |= is_user;
        protected |= is_protected;
        complete &= group_complete;
        tool |= group.is_some();
        spill |= is_spill;
        marker |= is_marker;
        context_trim |= is_trim;
        let at_boundary = turn.as_deref() == Some(current_turn.as_str())
            || latest_boundary.as_deref() == Some(id.as_str())
            || current_boundary.as_deref() == Some(id.as_str());
        if let Some(span) = merger.push(ordinal, group.as_deref(), last) {
            let index = groups.len();
            let is_tool = tool;
            let is_complete = complete;
            let is_protected = protected;
            let work = is_tool || spill || marker || context_trim;
            let spill_or_tool = is_tool || spill;
            groups.try_reserve(1).map_err(|_| overflow())?;
            groups.push((
                span,
                is_tool,
                is_complete,
                is_protected,
                work,
                spill_or_tool,
                user,
            ));
            if is_tool && is_complete {
                latest_tool = Some(index)
            }
            if at_boundary {
                boundary_group.get_or_insert(index);
            }
            if user && request_group.is_none() {
                request_group = Some(index)
            }
            tool = false;
            complete = true;
            protected = false;
            user = false;
            spill = false;
            marker = false;
            context_trim = false;
        }
        ordinal_next += 1;
    }
    drop(rows);
    drop(statement);
    if ordinal_next == 0 {
        return Ok(None);
    }
    let Some(latest) = latest_tool else {
        return Ok(None);
    };
    if !groups[latest].2 || groups[latest].3 {
        return Ok(None);
    }
    let request = request_group.or(boundary_group).ok_or_else(|| {
        StorageError::InvalidInput("active GC has no current-turn request boundary".into())
    })?;
    if request + 1 >= latest {
        return Ok(None);
    }
    let Some(start) = (request + 1..latest)
        .take_while(|index| groups[*index].2 && !groups[*index].3 && !groups[*index].6)
        .find(|index| groups[*index].4)
    else {
        return Ok(None);
    };
    let selected_end = latest;
    if !groups[start..latest].iter().any(|group| group.5) {
        return Ok(None);
    }
    let tail_range = Some(groups[latest].0.clone());
    let last = selected_end - 1;
    let range = groups[start].0.start..groups[last].0.end;
    let mut sources = Vec::new();
    sources
        .try_reserve_exact(range.end - range.start)
        .map_err(|_| overflow())?;
    let mut selected=tx.prepare("SELECT p.ordinal,n.id,n.payload_revision,n.turn_id,json_extract(n.payload_json,'$.group_id'),EXISTS(SELECT 1 FROM receipt q WHERE q.target_thread_id=?1 AND q.message_node_id=n.id AND q.state IN ('accepted','held')),CASE WHEN json_extract(n.payload_json,'$.group_id') IS NULL THEN 1 ELSE EXISTS(SELECT 1 FROM tool_group g WHERE g.id=json_extract(n.payload_json,'$.group_id') AND g.state='completed' AND NOT EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state!='completed' OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))) END,json_type(n.payload_json,'$.summary_format') IS NOT NULL,CASE WHEN n.payload_json IS NULL OR json_type(n.payload_json,'$.output') IS NOT NULL THEN 0 WHEN n.turn_id IS ?3 AND (json_extract(n.payload_json,'$.role')='user' OR json_extract(n.payload_json,'$.item.role')='user') THEN 1 ELSE 0 END FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal>=?2 AND p.ordinal<?4 ORDER BY p.ordinal")?;
    let stop = i64::try_from(range.end).map_err(|_| overflow())?;
    let begin = i64::try_from(range.start).map_err(|_| overflow())?;
    let facts = selected.query_map(params![thread, begin, current_turn, stop], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, bool>(5)?,
            r.get::<_, bool>(6)?,
            r.get::<_, bool>(7)?,
            r.get::<_, bool>(8)?,
        ))
    })?;
    let mut active_user_messages = Vec::new();
    for row in facts {
        let (
            ordinal,
            id,
            revision,
            turn,
            group_id,
            protected,
            group_complete,
            has_summary_marker,
            current_user,
        ) = row?;
        if current_user {
            active_user_messages.push(id.clone());
        }
        sources.push(GcPlannedSource {
            ordinal,
            id,
            revision,
            turn,
            group_id,
            protected,
            group_complete,
            has_summary_marker,
            current_turn_user: current_user,
        });
    }
    if sources.len() != range.end - range.start {
        return Err(StorageError::Schema(
            "active GC range changed during metadata selection".into(),
        ));
    }
    Ok(Some(GcRangePlan {
        range,
        sources,
        selection_revision: selection,
        profile_revision,
        nonappend_generation: generation,
        epoch,
        profile,
        model,
        effort,
        stage: stage.clone(),
        active_tail_range: tail_range,
        active_turn: true,
        spill_epoch_advanced,
        drop_reasoning: false,
        active_user_messages,
    }))
}

pub(crate) enum GcCaptureOutcome {
    NoTarget,
    Cancelled,
    Captured(CapturedSummaryInput),
}

/// Production GC entry: choose R and capture it under one writer transaction.
pub(crate) fn capture_ordinary_gc_prefix_with_receipt_tx(
    db: &mut Connection,
    thread: &str,
    stage: &super::attempt::GcStage,
    reason: &str,
    trigger: &Value,
    target_plan: &Value,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<GcCaptureOutcome, StorageError> {
    if stage.thread_id != thread {
        return Err(StorageError::InvalidInput(
            "GC receipt belongs to another Thread".into(),
        ));
    }
    capture_gc_plan_and_sources(db, thread, stage, reason, trigger, target_plan, cancel)
}

fn capture_gc_plan_and_sources(
    db: &mut Connection,
    thread: &str,
    stage: &super::attempt::GcStage,
    reason: &str,
    trigger: &Value,
    target_plan: &Value,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<GcCaptureOutcome, StorageError> {
    let tx = db.transaction()?;
    let planned = if trigger["scope"] == "active_turn" {
        let expected_turn = trigger["turn_id"].as_str().ok_or_else(|| {
            StorageError::InvalidInput("active GC trigger lacks original Turn provenance".into())
        })?;
        plan_gc_active_turn_tx(&tx, thread, stage, expected_turn)?
    } else {
        plan_gc_prefix_tx(&tx, thread, stage)?
    };
    let Some(planned) = planned else {
        tx.commit()?;
        return Ok(GcCaptureOutcome::NoTarget);
    };
    // Once R is known, explicit Stop can return a target fact without reading
    // source payloads or creating the provisional receipt/producer.
    if cancel.is_cancelled() {
        tx.commit()?;
        return Ok(GcCaptureOutcome::Cancelled);
    }
    let range = planned.range.clone();
    let start = i64::try_from(range.start).map_err(|_| overflow())?;
    let end = i64::try_from(range.end).map_err(|_| overflow())?;
    let range_len = range.end.checked_sub(range.start).ok_or_else(overflow)?;
    let thread_facts = tx.query_row(
        "SELECT selection_revision,profile_revision,selection_nonappend_generation,reasoning_epoch,profile_id,model_id,effort,summary_max_chars FROM thread WHERE id=?1 AND deleted_at IS NULL",
        [thread],
        |row| Ok((row.get::<_, i64>(0)?,row.get::<_, i64>(1)?,row.get::<_, i64>(2)?,row.get::<_, i64>(3)?,row.get::<_, String>(4)?,row.get::<_, String>(5)?,row.get::<_, Option<String>>(6)?,row.get::<_, Option<i64>>(7)?)),
    ).optional()?.ok_or_else(|| StorageError::from(ApiError::new(ErrorCode::ThreadNotFound, "GC summary target Thread is unavailable")))?;

    let rows:i64=tx.query_row(
        "SELECT count(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal>=?2 AND p.ordinal<?3",
        params![thread,start,end],|row|row.get(0))?;
    if usize::try_from(rows).ok() != Some(range_len) {
        return Err(StorageError::from(invalid(
            "Selected GC summary interval is no longer contiguous",
        )));
    }

    let mut row_plans = Vec::new();
    row_plans
        .try_reserve_exact(range_len)
        .map_err(|_| overflow())?;
    let mut statement = tx.prepare(
        "SELECT p.ordinal,n.id,n.turn_id,n.payload_revision,
            json_extract(n.payload_json,'$.group_id'),
            EXISTS(SELECT 1 FROM receipt q WHERE q.target_thread_id=?1 AND q.message_node_id=n.id AND q.state IN ('accepted','held')),
            json_type(n.payload_json,'$.summary_format') IS NOT NULL
         FROM thread_path p JOIN node n ON n.id=p.node_id
         WHERE p.thread_id=?1 AND p.ordinal>=?2 AND p.ordinal<?3 ORDER BY p.ordinal",
    )?;
    let meta_rows = statement.query_map(params![thread, start, end], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, bool>(5)?,
            row.get::<_, bool>(6)?,
        ))
    })?;
    for row in meta_rows {
        let (ordinal, id, turn, revision, group_id, protected, summary) = row?;
        let group_complete = if let Some(group) = &group_id {
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM tool_group g WHERE g.id=?1 AND g.state='completed' AND NOT EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state!='completed' OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id))))",
                [group], |r| r.get::<_,bool>(0),
            )?
        } else {
            true
        };
        row_plans.push(RowPlan {
            ordinal,
            id,
            turn,
            revision,
            group_id,
            complete: group_complete,
            protected,
            summary,
        });
    }
    drop(statement);
    if row_plans.len() != range_len {
        return Err(StorageError::from(invalid(
            "Selected GC summary interval changed during capture",
        )));
    }
    let plan = &planned;
    if plan.selection_revision != thread_facts.0
        || plan.profile_revision != thread_facts.1
        || plan.nonappend_generation != thread_facts.2
        || plan.epoch != thread_facts.3
        || plan.sources.len() != row_plans.len()
        || plan.sources.iter().zip(&row_plans).any(|(source, row)| {
            source.ordinal != row.ordinal
                || source.id != row.id
                || source.revision != row.revision
                || source.turn != row.turn
                || source.group_id != row.group_id
                || source.protected != row.protected
                || source.group_complete != row.complete
        })
    {
        return Err(StorageError::InvalidInput(
            "metadata GC range plan changed before payload capture".into(),
        ));
    }

    if native_tool_span_splits(&tx, thread, start, end)? {
        return Err(StorageError::from(invalid(
            "Selected GC summary interval splits a tool group",
        )));
    }
    let group_ranges =
        merge_source_group_ranges(row_plans.iter().map(|row| row.group_id.as_deref()));
    let mut source_characters = 0u64;
    let mut groups = Vec::new();
    groups
        .try_reserve_exact(group_ranges.len())
        .map_err(|_| overflow())?;
    let mut active_tail_group = None;
    for span in &group_ranges {
        if cancel.is_cancelled() {
            tx.commit()?;
            return Ok(GcCaptureOutcome::Cancelled);
        }
        if let (Some(active), Some(first)) = (
            planned.active_tail_range.as_ref(),
            row_plans.get(span.start),
        ) {
            if usize::try_from(first.ordinal).ok() == Some(active.start) {
                active_tail_group = Some(groups.len());
            }
        }
        let mut group = CapturedGroup {
            stamps: Vec::new(),
            text: String::new(),
            projection_weight: 0,
            has_unsummarized: false,
            complete: true,
            tool: false,
        };
        group
            .stamps
            .try_reserve_exact(span.len())
            .map_err(|_| overflow())?;
        for row in &row_plans[span.clone()] {
            if row.group_id.is_some() {
                group.complete &= row.complete;
                group.tool = true;
            }
        }
        groups.push(group);
    }
    let mut group_index = 0usize;
    for (index, row) in row_plans.into_iter().enumerate() {
        if cancel.is_cancelled() {
            tx.commit()?;
            return Ok(GcCaptureOutcome::Cancelled);
        }
        while group_index + 1 < group_ranges.len() && index >= group_ranges[group_index].end {
            group_index += 1;
        }
        let current = &mut groups[group_index];
        let mut payload_statement =
            tx.prepare("SELECT payload_json FROM node WHERE id=?1 AND payload_revision=?2")?;
        let mut payload_rows = payload_statement.query(params![row.id, row.revision])?;
        let payload_row = payload_rows
            .next()?
            .ok_or_else(|| StorageError::from(invalid("Selected source changed during capture")))?;
        let payload_ref = payload_row.get_ref(0)?;
        let payload: Value = match payload_ref {
            rusqlite::types::ValueRef::Null => {
                json!({"role":"developer","content":[{"type":"input_text","text":format!("Original node {} is unavailable",row.id)}]})
            }
            rusqlite::types::ValueRef::Text(bytes) => {
                serde_json::from_slice(bytes).map_err(|e| StorageError::Schema(e.to_string()))?
            }
            _ => return Err(StorageError::Schema("node payload_json is not text".into())),
        };
        drop(payload_rows);
        drop(payload_statement);
        let marker_adapter = super::summary_marker_item(&payload).is_some();
        let projection = super::edit::projection_characters(&payload, thread_facts.3);
        source_characters = source_characters
            .checked_add(projection)
            .ok_or_else(overflow)?;
        current.projection_weight = current
            .projection_weight
            .checked_add(u128::from(projection))
            .ok_or_else(overflow)?;
        current.has_unsummarized |= !row.summary;
        let hash = key(&(row.revision, &payload));
        let attachments = load_attachments(&tx, thread, &row.id)?;
        if !current.stamps.is_empty() {
            current.text.push_str("\n\n");
        }
        let _source_chars = render_into(
            RenderSourceView {
                id: &row.id,
                revision: row.revision,
                attachments: &attachments,
            },
            payload,
            thread_facts.3,
            &mut current.text,
        );
        current.stamps.push(CapturedSourceStamp {
            ordinal: row.ordinal,
            id: row.id.clone(),
            turn: row.turn.clone(),
            revision: row.revision,
            hash,
            marker_adapter,
            group_id: row.group_id.clone(),
            protected: row.protected,
            attachments,
        });
    }
    let request_turn = {
        let turn = trigger["turn_id"].as_str().ok_or_else(|| {
            StorageError::InvalidInput("GC trigger lacks request Turn provenance".into())
        })?;
        let generation: Option<i64> = tx
            .query_row(
                "SELECT generation FROM turn WHERE id=?1 AND thread_id=?2",
                params![turn, thread],
                |r| r.get(0),
            )
            .optional()?;
        let generation = generation.ok_or_else(|| {
            StorageError::InvalidInput("GC request Turn provenance disappeared".into())
        })?;
        Some((turn.to_owned(), generation))
    };
    let origin = {
        tx.query_row("SELECT t.id,t.run_id,t.generation FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE a.id=?1 AND t.thread_id=?2",params![stage.attempt_id,thread],|row|Ok((Some(row.get::<_,String>(0)?),Some(row.get::<_,String>(1)?),Some(row.get::<_,i64>(2)?)))).optional()?.ok_or_else(||StorageError::InvalidInput("GC parent attempt provenance disappeared".into()))?
    };
    {
        if cancel.is_cancelled() {
            tx.commit()?;
            return Ok(GcCaptureOutcome::Cancelled);
        }
        if stage.thread_id != thread
            || stage.profile_id != thread_facts.4
            || stage.model_id != thread_facts.5
            || stage.effort != thread_facts.6
            || (stage.reasoning_epoch != thread_facts.3
                && !(planned.spill_epoch_advanced
                    && stage.reasoning_epoch.checked_add(1) == Some(thread_facts.3)))
        {
            return Err(StorageError::InvalidInput(
                "GC parent receipt context differs from the captured Thread".into(),
            ));
        }
        super::attempt::begin_gc_tx(&tx, stage, reason, trigger, target_plan)?;
    }
    tx.commit()?;
    Ok(GcCaptureOutcome::Captured(CapturedSummaryInput {
        thread: thread.to_owned(),
        selection_revision: thread_facts.0,
        profile_revision: thread_facts.1,
        nonappend_generation: thread_facts.2,
        epoch: thread_facts.3,
        source_characters,
        profile: thread_facts.4,
        model: thread_facts.5,
        thread_summary_max_chars: thread_facts.7.and_then(|value| u64::try_from(value).ok()),
        effort: thread_facts.6,
        origin_turn: origin.0,
        origin_run: origin.1,
        origin_generation: origin.2,
        request_turn_id: request_turn.as_ref().map(|(turn, _)| turn.clone()),
        request_generation: request_turn.map(|(_, generation)| generation),
        groups,
        active_tail_group,
        active_turn: planned.active_turn,
        spill_epoch_advanced: planned.spill_epoch_advanced,
        drop_reasoning: planned.drop_reasoning,
        active_user_messages: planned.active_user_messages.clone(),
    }))
}

struct SourceGroupRangeMerger {
    start: usize,
    end: usize,
}
impl SourceGroupRangeMerger {
    fn new() -> Self {
        Self { start: 0, end: 0 }
    }
    fn push(&mut self, index: usize, group: Option<&str>, last: usize) -> Option<Range<usize>> {
        if index == self.start {
            self.end = self.start;
        }
        if group.is_some() {
            self.end = self.end.max(last);
        }
        if index == self.end {
            let range = self.start..index + 1;
            self.start = index + 1;
            self.end = self.start;
            Some(range)
        } else {
            None
        }
    }
}
fn merge_source_group_ranges<'a>(
    groups: impl IntoIterator<Item = Option<&'a str>>,
) -> Vec<Range<usize>> {
    let values = groups.into_iter().collect::<Vec<_>>();
    let mut last = HashMap::new();
    for (index, group) in values.iter().enumerate() {
        if let Some(id) = group {
            last.insert(*id, index);
        }
    }
    let mut merger = SourceGroupRangeMerger::new();
    let mut ranges = Vec::new();
    for (index, group) in values.iter().enumerate() {
        if let Some(range) = merger.push(
            index,
            *group,
            group.and_then(|id| last.get(id).copied()).unwrap_or(index),
        ) {
            ranges.push(range);
        }
    }
    ranges
}
fn native_tool_span_splits(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    start: i64,
    end: i64,
) -> Result<bool, StorageError> {
    tx.query_row("WITH linked(group_id,ordinal) AS (SELECT g.id,p.ordinal FROM tool_group g JOIN output_item o ON o.attempt_id=g.attempt_id JOIN thread_path p ON p.node_id=o.node_id AND p.thread_id=?1 WHERE o.item_type='function_call' AND o.node_id IS NOT NULL UNION ALL SELECT r.group_id,p.ordinal FROM tool_result r JOIN thread_path p ON p.node_id=r.node_id AND p.thread_id=?1),spans AS (SELECT group_id,min(ordinal) first_ordinal,max(ordinal) last_ordinal FROM linked GROUP BY group_id) SELECT EXISTS(SELECT 1 FROM spans WHERE first_ordinal<?3 AND last_ordinal>=?2 AND (first_ordinal<?2 OR last_ordinal>=?3))",params![thread,start,end],|row|row.get(0)).map_err(StorageError::from)
}

fn overflow() -> StorageError {
    ApiError::new(
        ErrorCode::InputLimit,
        "GC summary capture size arithmetic overflow",
    )
    .into()
}
#[derive(Clone, Debug, PartialEq)]
pub(super) struct GcSourceFacts {
    pub(super) ordinal: i64,
    pub(super) id: String,
    pub(super) revision: i64,
    pub(super) turn: Option<String>,
    pub(super) group_id: Option<String>,
    pub(super) protected: bool,
    pub(super) group_complete: bool,
    pub(super) attachments: Vec<Value>,
}

/// Batch-load the exact selected source facts used by both early writer
/// reconciliation and strict adoption. Attachments are returned in the same
/// ordered projection used by summary rendering; no Thread snapshot is made.
pub(super) fn selected_source_facts(
    db: &Connection,
    thread: &str,
    range: Range<i64>,
) -> Result<Vec<GcSourceFacts>, rusqlite::Error> {
    let mut statement=db.prepare("SELECT p.ordinal,n.id,n.payload_revision,n.turn_id,json_extract(n.payload_json,'$.group_id'),EXISTS(SELECT 1 FROM receipt q WHERE q.target_thread_id=?1 AND q.message_node_id=n.id AND q.state IN ('accepted','held')),CASE WHEN json_extract(n.payload_json,'$.group_id') IS NULL THEN 1 ELSE EXISTS(SELECT 1 FROM tool_group g WHERE g.id=json_extract(n.payload_json,'$.group_id') AND g.state='completed' AND NOT EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state!='completed' OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))) END,COALESCE((SELECT json_group_array(json_object('id',a.id,'path',a.absolute_path,'kind',a.input_kind,'state',COALESCE(a.state,'not_selected'),'reason',a.reason)) FROM (SELECT ar.id,ar.absolute_path,ar.input_kind,ar.created_at,se.state,se.reason FROM attachment_ref ar LEFT JOIN attachment_selection se ON se.attachment_id=ar.id AND se.thread_id=?1 WHERE ar.source_node_id=n.id ORDER BY ar.created_at,ar.id) a),'[]') FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal>=?2 AND p.ordinal<?3 ORDER BY p.ordinal")?;
    let rows = statement.query_map(params![thread, range.start, range.end], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, bool>(5)?,
            row.get::<_, bool>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    rows.map(|row| {
        let (ordinal, id, revision, turn, group_id, protected, group_complete, attachments) = row?;
        let attachments = serde_json::from_str(&attachments).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok(GcSourceFacts {
            ordinal,
            id,
            revision,
            turn,
            group_id,
            protected,
            group_complete,
            attachments,
        })
    })
    .collect()
}

/// One equality authority for both eager job cancellation and strict FINISH.
/// Early reconciliation may skip separate transactional context/receipt checks;
/// it never invents a different selected-source equivalence rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceFactPolicy {
    EarlyReconcile,
    StrictFinalAdoption,
}
/// One equality authority for early producer invalidation and strict FINISH.
/// Early validation uses the common captured facts; FINISH adds transactional
/// safe-to-adopt requirements through the policy argument.
pub(super) fn classify_selected_source_facts(
    expected: &[GcSourceFacts],
    actual: &[GcSourceFacts],
    policy: SourceFactPolicy,
) -> Result<(), super::discard::RejectionReason> {
    use super::discard::RejectionReason::SourceChanged;
    if expected.len() != actual.len() {
        return Err(SourceChanged);
    }
    for (expected, actual) in expected.iter().zip(actual) {
        if expected.ordinal != actual.ordinal
            || expected.id != actual.id
            || expected.revision != actual.revision
            || expected.turn != actual.turn
            || expected.group_id != actual.group_id
            || expected.protected != actual.protected
            || expected.group_complete != actual.group_complete
            || expected.attachments != actual.attachments
            || (policy == SourceFactPolicy::StrictFinalAdoption
                && (!actual.group_complete || actual.protected))
        {
            return Err(SourceChanged);
        }
    }
    Ok(())
}

pub(super) fn load_attachments(
    tx: &rusqlite::Connection,
    thread: &str,
    source: &str,
) -> Result<Vec<Value>, rusqlite::Error> {
    let mut attachments = Vec::new();
    let mut statement = tx.prepare(
        "SELECT a.id,a.absolute_path,a.input_kind,COALESCE(s.state,'not_selected'),s.reason FROM attachment_ref a LEFT JOIN attachment_selection s ON s.attachment_id=a.id AND s.thread_id=?2 WHERE a.source_node_id=?1 ORDER BY a.created_at,a.id",
    )?;
    let mut rows = statement.query(params![source, thread])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let path: String = row.get(1)?;
        let kind: String = row.get(2)?;
        let state: String = row.get(3)?;
        let reason: Option<String> = row.get(4)?;
        attachments.push(json!({"id":id,"path":path,"kind":kind,"state":state,"reason":reason}));
    }
    Ok(attachments)
}
