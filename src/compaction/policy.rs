//! Pure selection rules for manual compaction and its two overflow stages.
use super::*;
use std::ops::Range;

pub(super) fn spill_ranges(snapshot: &Snapshot, include_latest: bool) -> Vec<Range<usize>> {
    let latest = snapshot.groups.iter().rposition(|g| g.tool && g.complete);
    snapshot
        .groups
        .iter()
        .enumerate()
        .filter_map(|(ordinal, group)| {
            if !group.tool
                || !range::group_is_safe(snapshot, ordinal)
                || (!include_latest && latest == Some(ordinal))
                || group
                    .sources
                    .iter()
                    .all(|s| s.payload.get("spill_version").is_some())
            {
                None
            } else {
                Some(ordinal..ordinal + 1)
            }
        })
        .collect()
}
pub(super) fn summary_range(snapshot: &Snapshot, allow_suffix: bool) -> Option<Range<usize>> {
    let count = prefix_count(snapshot, allow_suffix);
    (count > 0).then_some(0..count)
}

pub(super) fn prefix_count(s: &Snapshot, allow_suffix: bool) -> usize {
    if !allow_suffix {
        return normal_prefix(s);
    }
    overflow_prefix(s, completed_prefix(s))
}
pub(super) fn normal_prefix(s: &Snapshot) -> usize {
    normal_boundary(s).min(safe_prefix(s))
}
pub(super) fn completed_prefix(s: &Snapshot) -> usize {
    // Release the latest completed Turn before considering current-Turn groups.
    let current = turn_boundary(s, s.current_turn.as_deref());
    current
        .unwrap_or(s.groups.len())
        .min(safe_prefix(s))
        .min(s.groups.len().saturating_sub(1))
}
pub(super) fn overflow_prefix(s: &Snapshot, completed: usize) -> usize {
    // This is the existing summary segment sizing baseline, not an estimate of
    // provider context capacity. Release only the oldest whole groups needed
    // to bring the remaining suffix below it, without crossing a hard fence.
    let safe = safe_prefix(s).min(s.groups.len().saturating_sub(1));
    if completed >= safe {
        return completed;
    }
    let sizes: Vec<_> = s.groups[completed..]
        .iter()
        .map(|g| {
            g.sources
                .iter()
                .map(|n| render(n, s.epoch).chars().count())
                .sum::<usize>()
        })
        .collect();
    let mut remaining = sizes.iter().sum::<usize>();
    let mut count = completed;
    if completed <= normal_prefix(s) && completed < safe {
        // Explicit overflow still requires progress below 64K: provider limits
        // may be smaller. Do not count summarizing just the origin user as
        // progress when an older complete tool group (or its spill) is eligible.
        let first_tool = s.groups[completed..safe].iter().position(|g| {
            g.tool
                || g.sources
                    .iter()
                    .any(|n| n.payload.get("spill_version").is_some())
        });
        count += first_tool.map_or(1, |index| index + 1);
        remaining = remaining.saturating_sub(sizes[..count - completed].iter().sum());
    }
    while count < safe && remaining > SEGMENT_CHARS {
        remaining = remaining.saturating_sub(sizes[count - completed]);
        count += 1;
    }
    count
}
fn turn_user(group: &Group, id: &str) -> bool {
    group.sources.iter().any(|n| {
        n.turn.as_deref() == Some(id)
            && (n.payload["role"] == "user"
                || n.payload.pointer("/item/role").is_some_and(|r| r == "user"))
    })
}
pub(super) fn request_boundary(s: &Snapshot, turn: Option<&str>) -> Option<usize> {
    let id = turn?;
    // A marker authored during this Turn must not displace its selected user request.
    if let Some(index) = s.groups.iter().position(|g| turn_user(g, id)) {
        return Some(index);
    }
    turn_boundary(s, turn)
}
/// Active-Turn middle segment (#181): the oldest run of whole groups after the
/// current request and before the latest complete tool group, which starts the
/// retained recent working suffix. Incomplete or protected groups and every
/// user message of the active Turn are fences. A run qualifies only if it holds
/// a tool group or spill marker, so a previous summary alone is never a target
/// and an unchanged long Turn does not re-fire (#180).
pub(super) fn active_turn_range(s: &Snapshot) -> Option<Range<usize>> {
    let turn = s.current_turn.as_deref()?;
    let request = request_boundary(s, Some(turn))?;
    let latest = s.groups.iter().rposition(|g| g.complete && g.tool)?;
    let fence = |i: usize| !range::group_is_safe(s, i) || turn_user(&s.groups[i], turn);
    let mut start = request + 1;
    while start < latest {
        if fence(start) {
            start += 1;
            continue;
        }
        let end = (start..latest).find(|&i| fence(i)).unwrap_or(latest);
        if s.groups[start..end].iter().any(|g| {
            g.tool
                || g.sources
                    .iter()
                    .any(|n| n.payload.get("spill_version").is_some())
        }) {
            return Some(start..end);
        }
        start = end;
    }
    None
}
pub(super) fn turn_boundary(s: &Snapshot, turn: Option<&str>) -> Option<usize> {
    let turn = turn?;
    let boundary = s.turn_boundaries.get(turn);
    s.groups.iter().position(|g| {
        g.sources
            .iter()
            .any(|n| n.turn.as_deref() == Some(turn) || boundary.is_some_and(|id| *id == n.id))
    })
}
pub(super) fn normal_boundary(s: &Snapshot) -> usize {
    [s.current_turn.as_deref(), s.latest_complete_turn.as_deref()]
        .into_iter()
        .filter_map(|turn| turn_boundary(s, turn))
        .min()
        .unwrap_or(s.groups.len())
}
pub(super) fn safe_prefix(s: &Snapshot) -> usize {
    let latest_tool = s.groups.iter().rposition(|g| g.complete && g.tool);
    s.groups
        .iter()
        .enumerate()
        .position(|(i, _)| !range::group_is_safe(s, i) || Some(i) == latest_tool)
        .unwrap_or(s.groups.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::tests::staged_snapshot;

    #[test]
    fn a_same_turn_marker_cannot_replace_the_selected_user_request_fence() {
        let mut s = staged_snapshot();
        s.groups[0].sources[0].turn = Some("current".into());
        s.groups[0].sources[0].payload =
            serde_json::json!({"item":{"role":"developer","content":[]}});
        s.groups[1].sources[0].turn = Some("current".into());
        s.groups[1].sources[0].payload = serde_json::json!({"role":"user","content":"protected"});
        s.turn_boundaries
            .insert("current".into(), s.groups[0].sources[0].id.clone());
        assert_eq!(request_boundary(&s, Some("current")), Some(1));
    }

    #[test]
    fn ordinary_summary_excludes_long_current_turn_even_after_spilling() {
        let mut s = staged_snapshot();
        s.groups[0].sources[0].payload = json!({"role":"assistant","content":"small prefix"});
        s.groups[3].sources[1].payload = json!({"text":"x".repeat(800_000)});
        assert_eq!(summary_range(&s, false), Some(0..1));
        assert_eq!(spill_ranges(&s, false), vec![3..4]);
        // A spill replacement keeps its selected position; it does not move
        // the protected current/latest-completed Turn summary boundary.
        s.groups[3].tool = false;
        s.groups[3].sources.truncate(1);
        s.groups[3].sources[0].turn = None;
        s.groups[3].sources[0].payload = json!({"spill_version":1,"text":"omitted output"});
        assert_eq!(summary_range(&s, false), Some(0..1));
        assert!(spill_ranges(&s, false).is_empty());
    }

    #[test]
    fn active_turn_range_keeps_request_user_messages_and_working_suffix() {
        let mut s = staged_snapshot();
        // request "current"(2), old tool group(3), spill(4), latest tool(5)
        assert_eq!(active_turn_range(&s), Some(3..5));
        // A later user message of the active Turn is a fence.
        s.groups[2].sources[0].payload = json!({"role":"user","content":"request"});
        assert_eq!(active_turn_range(&s), Some(3..5));
        s.groups[4].sources[0].turn = Some("current".into());
        s.groups[4].sources[0].payload = json!({"role":"user","content":"more"});
        assert_eq!(active_turn_range(&s), Some(3..4));
        // Incomplete groups are fences; a run without any tool group or
        // spill marker (e.g. a previous summary alone) is no target.
        let mut s = staged_snapshot();
        s.groups[3].complete = false;
        assert_eq!(active_turn_range(&s), None);
        s.groups[4].sources[0].payload = json!({"spill_version":1,"output":[]});
        assert_eq!(active_turn_range(&s), Some(4..5));
        s.groups[4].sources[0].payload = json!({"item":{"role":"developer","content":[]}});
        assert_eq!(active_turn_range(&s), None);
        let mut s = staged_snapshot();
        s.protected.insert("a-result".into());
        assert_eq!(active_turn_range(&s), None);
        // The latest complete tool group is never covered; no active Turn or
        // no complete tool group after the request means no target.
        let mut s = staged_snapshot();
        s.groups.truncate(4);
        assert_eq!(active_turn_range(&s), None);
        let mut s = staged_snapshot();
        s.current_turn = None;
        assert_eq!(active_turn_range(&s), None);
    }

    #[test]
    fn manual_and_overflow_policy_tables_keep_existing_group_ranges() {
        let mut snapshot = staged_snapshot();
        assert_eq!(summary_range(&snapshot, false), Some(0..1));
        assert_eq!(summary_range(&snapshot, true), Some(0..4));
        for (latest, expected) in [
            (false, std::iter::once(3..4).collect()),
            (true, vec![3..4, 5..6]),
        ] {
            assert_eq!(spill_ranges(&snapshot, latest), expected);
        }
        snapshot.protected.insert("a-result".into());
        assert!(spill_ranges(&snapshot, false).is_empty());
        assert_eq!(spill_ranges(&snapshot, true), vec![5..6]);
        assert_eq!(summary_range(&snapshot, true), Some(0..3));
        snapshot.protected.clear();
        snapshot.groups[5].complete = false;
        assert!(spill_ranges(&snapshot, false).is_empty());
        assert_eq!(spill_ranges(&snapshot, true), vec![3..4]);
        for source in &mut snapshot.groups[3].sources {
            source.payload["spill_version"] = json!(1);
        }
        assert!(spill_ranges(&snapshot, true).is_empty());
    }
}
