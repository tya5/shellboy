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
/// Automatic stages distinguish older Turns from the active Turn while
/// retaining the shared latest-tool and whole-group safety checks.
pub(super) fn staged_spill_ranges(snapshot: &Snapshot, active: bool) -> Vec<Range<usize>> {
    let boundary = request_boundary(snapshot, snapshot.current_turn.as_deref())
        .unwrap_or(snapshot.groups.len());
    spill_ranges(snapshot, false)
        .into_iter()
        .filter(|range| {
            if active {
                range.start > boundary
            } else {
                range.end <= boundary
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
        // Explicit overflow still requires progress below the segment size: provider limits
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
    while count < safe && remaining > s.segment_chars {
        remaining = remaining.saturating_sub(sizes[count - completed]);
        count += 1;
    }
    count
}
/// Whether a group holds a user message of Turn `id` (its request or a message
/// delivered later in it).
pub(super) fn turn_user(group: &Group, id: &str) -> bool {
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
/// #182 active-Turn summary plan. Everything before `start` (earlier Turns,
/// the request and anything else ahead of the first tool round) stays
/// byte-identical. Groups `start..latest` (③) are summarized; the latest
/// complete tool group `latest` (④) is appended to the same summary as plain
/// text. `messages` are the active Turn's user messages in that span: they
/// leave the selected path with the rounds and the summary keeps what must
/// stay in effect under "User instructions to keep" (user decision B).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ActiveTurnPlan {
    pub(super) start: usize,
    pub(super) latest: usize,
    pub(super) messages: Vec<String>,
}
impl ActiveTurnPlan {
    pub(super) fn end(&self) -> usize {
        self.latest + 1
    }
}
fn work_group(group: &Group) -> bool {
    group.tool
        || group.sources.iter().any(|n| {
            n.payload.get("spill_version").is_some()
                || n.payload.get("summary_scope").is_some()
                || n.payload.get("context_trim").is_some()
        })
}
fn spill_or_tool(group: &Group) -> bool {
    group.tool
        || group
            .sources
            .iter()
            .any(|n| n.payload.get("spill_version").is_some())
}
/// A user message of `turn` inside the span.
pub(super) fn turn_message(source: &Source, turn: &str) -> bool {
    if source.payload.get("output").is_some() {
        return false;
    }
    source.turn.as_deref() == Some(turn)
        && (source.payload["role"] == "user"
            || source
                .payload
                .pointer("/item/role")
                .is_some_and(|r| r == "user"))
}
/// Active-Turn summary target (#181, reshaped by #182): all tool rounds of the
/// active Turn from its first tool round through the latest complete tool
/// group. No target unless ③ holds a tool group or spill marker (a previous
/// summary alone never re-fires, #180), or if any covered group is incomplete.
pub(super) fn active_turn_plan(s: &Snapshot) -> Option<ActiveTurnPlan> {
    let turn = s.current_turn.as_deref()?;
    let request = request_boundary(s, Some(turn))?;
    let latest = s.groups.iter().rposition(|g| g.complete && g.tool)?;
    let start = (request + 1..latest).find(|&i| work_group(&s.groups[i]))?;
    let span = &s.groups[start..=latest];
    // Protected nodes (pending receipts, revisit events) keep their own
    // lifecycle; a span holding one is not a target.
    if span.iter().any(|g| !g.complete)
        || span
            .iter()
            .flat_map(|g| &g.sources)
            .any(|n| s.protected.contains(&n.id))
        || !span[..span.len() - 1].iter().any(spill_or_tool)
    {
        return None;
    }
    let messages = span
        .iter()
        .flat_map(|g| &g.sources)
        .filter(|source| turn_message(source, turn))
        .map(|source| source.id.clone())
        .collect();
    Some(ActiveTurnPlan {
        start,
        latest,
        messages,
    })
}
/// Oldest safe active-Turn work groups, stopping at the calibrated need.
pub(super) fn target_active_range(
    s: &Snapshot,
    target: u64,
    config: &crate::config::Compaction,
) -> Option<(Range<usize>, u64)> {
    let turn = s.current_turn.as_deref()?;
    let request = request_boundary(s, Some(turn))?;
    let latest = s.groups.iter().rposition(|g| g.tool && g.complete)?;
    if latest <= request + 1 {
        return None;
    }
    let start = (request + 1..latest)
        .take_while(|&i| {
            range::group_is_safe(s, i) && !s.groups[i].sources.iter().any(|n| turn_message(n, turn))
        })
        .find(|&i| work_group(&s.groups[i]))?;
    let mut chars = 0u64;
    let mut end = start;
    let mut has_work = false;
    for i in start..latest {
        let group = &s.groups[i];
        if !range::group_is_safe(s, i) || group.sources.iter().any(|n| turn_message(n, turn)) {
            break;
        }
        has_work |= spill_or_tool(group);
        chars = group
            .sources
            .iter()
            .map(|n| edit::projection_characters(&n.payload, s.epoch))
            .fold(chars, u64::saturating_add);
        end = i + 1;
        if has_work && chars.saturating_sub(config.summary_limit(chars)) >= target {
            break;
        }
    }
    if !has_work {
        return None;
    }
    let removal = chars.saturating_sub(config.summary_limit(chars));
    Some((start..end, target.saturating_sub(removal)))
}

/// Ordinary prefix target for the proactive order (#182): `None` when the
/// prefix holds only earlier summaries, so a summary is never re-summarized
/// alone before the active Turn is considered.
pub(super) fn unsummarized_prefix(s: &Snapshot) -> Option<Range<usize>> {
    summary_range(s, false).filter(|range| {
        s.groups[range.clone()]
            .iter()
            .flat_map(|g| &g.sources)
            .any(|n| n.payload.get("summary_format").is_none())
    })
}
pub(super) fn target_prefix(
    s: &Snapshot,
    target: u64,
    config: &crate::config::Compaction,
) -> Option<(Range<usize>, u64)> {
    let safe = unsummarized_prefix(s)?;
    let mut characters = 0u64;
    let mut has_unsummarized = false;
    for end in 1..=safe.end {
        for source in &s.groups[end - 1].sources {
            characters =
                characters.saturating_add(edit::projection_characters(&source.payload, s.epoch));
            has_unsummarized |= source.payload.get("summary_format").is_none();
        }
        let removal = characters.saturating_sub(config.summary_limit(characters));
        if has_unsummarized && (removal >= target || end == safe.end) {
            return Some((0..end, target.saturating_sub(removal)));
        }
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
    fn active_target_window_stops_and_keeps_user_and_tool_fences() {
        let mut s = staged_snapshot();
        s.groups[3].sources[0].payload = json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"x".repeat(10_000)}]}]});
        let mut another = s.groups[3].clone();
        another.sources[0].id = "second-tool".into();
        s.groups.insert(4, another);
        let config = crate::config::Compaction::default();
        let (range, missing) = target_active_range(&s, 4_000, &config).unwrap();
        assert_eq!(range, 3..4);
        assert_eq!(missing, 0);
        let (range, missing) = target_active_range(&s, 100_000, &config).unwrap();
        assert_eq!(range, 3..6);
        assert!(missing > 0);
        s.groups[4].sources[0].payload = json!({"role":"user","content":"keep instruction"});
        assert!(target_active_range(&s, 100_000, &config).is_none());
        s.groups[3].complete = false;
        assert!(target_active_range(&s, 1, &config).is_none());
    }

    #[test]
    fn staged_spill_keeps_turn_and_group_fences() {
        let mut s = staged_snapshot();
        s.groups[0].tool = true;
        assert_eq!(staged_spill_ranges(&s, false), vec![0..1]);
        assert_eq!(staged_spill_ranges(&s, true), vec![3..4]);
        s.groups[3].complete = false;
        assert!(staged_spill_ranges(&s, true).is_empty());
        s.groups[3].complete = true;
        s.protected.insert("a-result".into());
        assert!(staged_spill_ranges(&s, true).is_empty());
    }

    #[test]
    fn target_window_stops_at_whole_group_and_reports_shortfall() {
        let mut s = staged_snapshot();
        let template = s.groups[0].clone();
        let mut old = Vec::new();
        for i in 0..8 {
            let mut group = template.clone();
            group.sources[0].id = format!("old-{i}");
            group.sources[0].payload = json!({"role":"user","content":"x".repeat(10_000)});
            old.push(group);
        }
        old.extend(s.groups[1..].iter().cloned());
        s.groups = old;
        let config = crate::config::Compaction::default();
        let (range, shortfall) = target_prefix(&s, 25_000, &config).unwrap();
        assert_eq!(range, 0..3);
        assert_eq!(shortfall, 0);
        let (range, shortfall) = target_prefix(&s, 100_000, &config).unwrap();
        assert_eq!(range, 0..8);
        assert!(shortfall > 0);
        s.groups[2].complete = false;
        let (range, shortfall) = target_prefix(&s, 100_000, &config).unwrap();
        assert_eq!(range, 0..2);
        assert!(shortfall > 0);
    }

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
    fn active_turn_plan_covers_every_tool_round_and_retains_user_inputs() {
        let s = staged_snapshot();
        // request "current"(2), old tool group(3), spill(4), latest tool(5):
        // ③ = 3..5 summarized, ④ = 5 as text, nothing before 3 changes.
        let plan = active_turn_plan(&s).unwrap();
        assert_eq!((plan.start, plan.latest, plan.end()), (3, 5, 6));
        assert!(plan.messages.is_empty());
        // A later user message of the active Turn inside the span is kept
        // verbatim (summarized), no longer a fence.
        let mut s = staged_snapshot();
        s.groups[2].sources[0].payload = json!({"role":"user","content":"request"});
        s.groups[4].sources[0].turn = Some("current".into());
        s.groups[4].sources[0].payload = json!({"role":"user","content":"more"});
        let plan = active_turn_plan(&s).unwrap();
        assert_eq!((plan.start, plan.latest), (3, 5));
        assert_eq!(plan.messages, [s.groups[4].sources[0].id.clone()]);
        // A protected node in the span means no target.
        let mut s = staged_snapshot();
        s.protected.insert("a-result".into());
        assert_eq!(active_turn_plan(&s), None);
        // Incomplete groups: no target. ③ without a tool group or spill (a
        // previous summary alone) is no target.
        let mut s = staged_snapshot();
        s.groups[3].complete = false;
        assert_eq!(active_turn_plan(&s), None);
        let mut s = staged_snapshot();
        s.groups[3].tool = false;
        s.groups[3].sources.truncate(1);
        s.groups[3].sources[0].payload =
            json!({"summary_scope":"active_turn","item":{"role":"developer","content":[]}});
        s.groups[4].sources[0].payload = json!({"item":{"role":"developer","content":[]}});
        assert_eq!(active_turn_plan(&s), None);
        // No active Turn or no complete tool round after the request.
        let mut s = staged_snapshot();
        s.groups.truncate(4);
        assert_eq!(active_turn_plan(&s), None);
        let mut s = staged_snapshot();
        s.current_turn = None;
        assert_eq!(active_turn_plan(&s), None);
    }

    #[test]
    fn unsummarized_prefix_ignores_a_prefix_made_only_of_summaries() {
        let mut s = staged_snapshot();
        assert_eq!(unsummarized_prefix(&s), Some(0..1));
        s.groups[0].sources[0].payload = json!({"summary_format":"shellboy-summary-v1","item":{"role":"developer","content":[]}});
        assert_eq!(unsummarized_prefix(&s), None);
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
