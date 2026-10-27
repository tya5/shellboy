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
                || !group.complete
                || (!include_latest && latest == Some(ordinal))
                || group
                    .sources
                    .iter()
                    .any(|s| snapshot.protected.contains(&s.id))
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
        .position(|(i, g)| {
            !g.complete
                || Some(i) == latest_tool
                || g.sources.iter().any(|n| s.protected.contains(&n.id))
        })
        .unwrap_or(s.groups.len())
}
