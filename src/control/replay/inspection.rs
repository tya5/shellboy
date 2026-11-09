//! Read-only diagnostics over exactly the frames selected for this inspection.
use super::*;
use crate::protocol::{ApiError, ErrorCode};
use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
struct Group<'a> {
    calls: HashSet<&'a str>,
    outputs: HashSet<&'a str>,
    anchor: Option<&'a str>,
    output_nodes: HashMap<&'a str, &'a str>,
}

pub(super) fn unassociated_fault() -> StorageError {
    invalid("metadata_unresolved")
}

fn invalid(fault: &'static str) -> StorageError {
    diagnostic("request_history_invalid", fault)
}
fn diagnostic(reason: &'static str, fault: &'static str) -> StorageError {
    let hint = if reason == "request_history_busy" {
        "The selected tool group is recorded active; retry inspection after it settles. Recorded activity does not prove a driver is alive."
    } else {
        "Inspect the selected saved history and recovery guidance; do not automatically retry or repair it."
    };
    ApiError::new(
        ErrorCode::InvalidState,
        "Selected request history cannot be assembled",
    )
    .hint(hint)
    .details(serde_json::json!({"reason":reason,"fault":fault}))
    .into()
}

pub(super) fn validate(
    db: &Connection,
    thread: &str,
    frames: &[Arc<Frame>],
) -> Result<(), StorageError> {
    let mut groups: HashMap<Option<&str>, Group<'_>> = HashMap::new();
    // Finish all structural checks before interpreting any recorded activity.
    let mut ungrouped = Vec::new();
    for frame in frames {
        if frame.group.is_none() {
            ungrouped.push(frame.clone());
            continue;
        }
        if frame
            .items
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
            && frame.items.iter().any(|item| {
                item.get("type").and_then(Value::as_str) != Some("function_call_output")
            })
        {
            return Err(invalid("mismatch"));
        }
        for item in &frame.items {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            if !matches!(kind, "function_call" | "function_call_output") {
                continue;
            }
            let id = item
                .get("call_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| invalid("mismatch"))?;
            let group = groups.entry(frame.group.as_deref()).or_default();
            if kind == "function_call" {
                if group.anchor.is_some_and(|anchor| anchor != frame.stamp.id)
                    || !group.calls.insert(id)
                {
                    return Err(invalid("duplicate"));
                }
                group.anchor = Some(&frame.stamp.id);
            } else {
                if !group.outputs.insert(id) {
                    return Err(invalid("duplicate"));
                }
                group.output_nodes.insert(id, &frame.stamp.id);
            }
        }
    }
    tool_order::ordered_checked(ungrouped).map_err(|_| invalid("metadata_unresolved"))?;
    for group in groups.values() {
        if !group.outputs.is_subset(&group.calls) {
            return Err(invalid("orphan"));
        }
    }
    let missing: Vec<_> = groups
        .iter()
        .filter(|(_, g)| g.calls != g.outputs)
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut busy = false;
    let mut incomplete = false;
    let mut settled = false;
    for (id, group) in missing {
        let Some(id) = id else {
            return Err(invalid("metadata_unresolved"));
        };
        let row = db.query_row(
            "SELECT g.state,g.turn_id,a.turn_id,a.generation,a.state,u.generation,u.state,u.thread_id,r.thread_id,r.state,n.turn_id FROM tool_group g JOIN model_attempt a ON a.id=g.attempt_id JOIN turn u ON u.id=g.turn_id JOIN run r ON r.id=u.run_id JOIN node n ON n.id=?2 WHERE g.id=?1",
            rusqlite::params![id,group.anchor], |row| Ok((
                row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,
                row.get::<_,i64>(3)?,row.get::<_,String>(4)?,row.get::<_,i64>(5)?,
                row.get::<_,String>(6)?,row.get::<_,String>(7)?,row.get::<_,String>(8)?,
                row.get::<_,String>(9)?,row.get::<_,Option<String>>(10)?
            ))) .optional()?;
        let Some((gs, gt, at, ag, ast, ug, us, ut, rt, rs, nt)) = row else {
            return Err(invalid("metadata_unresolved"));
        };
        if gt != at
            || nt.as_deref() != Some(&gt)
            || ut != thread
            || rt != thread
            || ag > ug
            || ast != "completed"
        {
            return Err(invalid("metadata_inconsistent"));
        }
        // Associate the selected call anchor with this attempt, not another
        // attempt in the same Turn. Do not fetch missing result bodies.
        let associated: bool = db.query_row("SELECT (SELECT node_id FROM output_item WHERE attempt_id=(SELECT attempt_id FROM tool_group WHERE id=?1) ORDER BY ordinal LIMIT 1) IS ?2", rusqlite::params![id,group.anchor], |r| r.get(0))?;
        if !associated {
            return Err(invalid("metadata_unresolved"));
        }
        let mut statement = db
            .prepare("SELECT call_id FROM tool_call WHERE group_id=?1 ORDER BY ordinal LIMIT ?2")?;
        let recorded = statement
            .query_map(
                rusqlite::params![
                    id,
                    i64::try_from(group.calls.len())
                        .unwrap_or(i64::MAX)
                        .saturating_add(1)
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<HashSet<_>, _>>()?;
        if recorded.len() != group.calls.len()
            || recorded.iter().any(|id| !group.calls.contains(id.as_str()))
        {
            return Err(invalid("metadata_inconsistent"));
        }
        for (call, node) in &group.output_nodes {
            let recorded: Option<String> = db
                .query_row(
                    "SELECT node_id FROM tool_result WHERE group_id=?1 AND call_id=?2",
                    rusqlite::params![id, call],
                    |r| r.get(0),
                )
                .optional()?;
            if recorded.as_deref() != Some(node) {
                return Err(invalid("metadata_inconsistent"));
            }
        }
        match (gs.as_str(), us.as_str(), rs.as_str()) {
            ("running", "running" | "waiting", "live") if ag == ug => busy = true,
            ("running" | "completed", "stopped" | "recovery_wait" | "failed", _)
            | ("running" | "completed", _, "stopping" | "stopped" | "failed") => incomplete = true,
            ("completed", "completed", "completed" | "live") => settled = true,
            _ => return Err(invalid("metadata_inconsistent")),
        }
    }
    if settled {
        Err(invalid("missing_outputs"))
    } else if incomplete {
        Err(diagnostic("request_history_incomplete", "missing_outputs"))
    } else if busy {
        Err(diagnostic("request_history_busy", "missing_outputs"))
    } else {
        Err(invalid("metadata_unresolved"))
    }
}
