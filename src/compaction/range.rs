//! Whole-group range validation shared by every compaction selection policy.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum RangeMode {
    Spill,
    Summary,
    Trim,
}
fn group_error(snapshot: &Snapshot, index: usize) -> Option<&'static str> {
    let group = &snapshot.groups[index];
    if !group.complete {
        Some("Compaction range includes an incomplete or running tool group")
    } else if group
        .sources
        .iter()
        .any(|source| snapshot.protected.contains(&source.id))
    {
        Some("Compaction range includes a protected node")
    } else {
        None
    }
}
pub(super) fn group_is_safe(snapshot: &Snapshot, index: usize) -> bool {
    group_error(snapshot, index).is_none()
}
pub(super) fn trim_group_protected(
    index: usize,
    count: usize,
    current_request: Option<usize>,
) -> bool {
    index + 1 == count || Some(index) == current_request
}
fn range_error(
    snapshot: &Snapshot,
    message: &str,
    around: Option<usize>,
    mode: Option<RangeMode>,
) -> ApiError {
    // Report nearby whole-group endpoints, without an unbounded path dump.
    let start = around.unwrap_or(0).saturating_sub(8);
    let trimming = matches!(mode, Some(RangeMode::Trim));
    let current_request = if trimming {
        policy::request_boundary(snapshot, snapshot.current_turn.as_deref())
    } else {
        None
    };
    let candidates = snapshot
        .groups
        .iter()
        .enumerate()
        .skip(start)
        .take(16)
        .filter(|(index, _)| {
            group_is_safe(snapshot, *index)
                && (!trimming
                    || !trim_group_protected(*index, snapshot.groups.len(), current_request))
        })
        .filter_map(|(_, group)| {
            Some(json!({"from_node_id":group.sources.first()?.id,
            "to_node_id":group.sources.last()?.id}))
        })
        .collect::<Vec<_>>();
    ApiError::new(ErrorCode::InvalidArgument, message).details(json!({
        "valid_boundaries":candidates,"group_count":snapshot.groups.len(),
        "boundary_candidates_limited":snapshot.groups.len()>16
    }))
}
/// Group indexes cannot describe half a tool group. Reject bad bounds and
/// overlapping/unsorted spans rather than silently moving either boundary.
pub(super) fn validate(
    snapshot: &Snapshot,
    ranges: &[Range<usize>],
    mode: RangeMode,
) -> Result<(), ApiError> {
    let current_request = policy::request_boundary(snapshot, snapshot.current_turn.as_deref());
    let mut previous_end = 0;
    for (ordinal, range) in ranges.iter().enumerate() {
        if range.start >= range.end || range.end > snapshot.groups.len() {
            return Err(range_error(
                snapshot,
                "Compaction range must contain whole selected groups",
                Some(range.start),
                Some(mode),
            ));
        }
        if ordinal > 0 && range.start < previous_end {
            return Err(range_error(
                snapshot,
                "Compaction ranges must be ordered and must not overlap",
                Some(range.start),
                Some(mode),
            ));
        }
        for index in range.clone() {
            if let Some(message) = group_error(snapshot, index) {
                return Err(range_error(snapshot, message, Some(index), Some(mode)));
            }
            if matches!(mode, RangeMode::Trim)
                && trim_group_protected(index, snapshot.groups.len(), current_request)
            {
                return Err(range_error(
                    snapshot,
                    "Context trimming must retain the latest group and current request",
                    Some(index),
                    Some(mode),
                ));
            }
        }
        previous_end = range.end;
    }
    Ok(())
}
/// CLI endpoints are inclusive source-node IDs on the selected path. Neither
/// endpoint can select only part of the group's complete call/result span.
pub(super) fn from_nodes(
    snapshot: &Snapshot,
    from: &str,
    to: &str,
) -> Result<Range<usize>, ApiError> {
    from_nodes_with_mode(snapshot, from, to, None)
}
pub(super) fn from_nodes_for_mode(
    snapshot: &Snapshot,
    from: &str,
    to: &str,
    mode: RangeMode,
) -> Result<Range<usize>, ApiError> {
    from_nodes_with_mode(snapshot, from, to, Some(mode))
}
fn from_nodes_with_mode(
    snapshot: &Snapshot,
    from: &str,
    to: &str,
    mode: Option<RangeMode>,
) -> Result<Range<usize>, ApiError> {
    let start = snapshot
        .groups
        .iter()
        .position(|group| group.sources.iter().any(|source| source.id == from));
    let end = snapshot
        .groups
        .iter()
        .position(|group| group.sources.iter().any(|source| source.id == to));
    let (Some(start), Some(end)) = (start, end) else {
        return Err(range_error(
            snapshot,
            "Compaction endpoints must be nodes on the selected path",
            start.or(end),
            mode,
        ));
    };
    if start > end
        || snapshot.groups[start]
            .sources
            .first()
            .is_none_or(|source| source.id != from)
        || snapshot.groups[end]
            .sources
            .last()
            .is_none_or(|source| source.id != to)
    {
        return Err(range_error(
            snapshot,
            "Compaction endpoints must match whole-group boundaries in path order",
            Some(start),
            mode,
        ));
    }
    Ok(start..end + 1)
}

/// `--trim` can only select a prefix. Its recovery candidates must not merely
/// be safe individual groups after an intervening protected current request.
pub(super) fn from_prefix_end(snapshot: &Snapshot, to: &str) -> Result<Range<usize>, ApiError> {
    let from = snapshot
        .groups
        .first()
        .and_then(|group| group.sources.first())
        .ok_or_else(|| invalid("Cannot trim an empty path"))?;
    let selected =
        from_nodes_for_mode(snapshot, &from.id, to, RangeMode::Trim).and_then(|selected| {
            validate(snapshot, std::slice::from_ref(&selected), RangeMode::Trim)?;
            Ok(selected)
        });
    selected.map_err(|mut error| {
        let request = policy::request_boundary(snapshot, snapshot.current_turn.as_deref());
        let candidates = snapshot
            .groups
            .iter()
            .enumerate()
            .take_while(|(index, _)| {
                group_is_safe(snapshot, *index)
                    && !trim_group_protected(*index, snapshot.groups.len(), request)
            })
            .take(16)
            .filter_map(|(_, group)| {
                group
                    .sources
                    .last()
                    .map(|last| json!({"from_node_id":from.id,"to_node_id":last.id}))
            })
            .collect::<Vec<_>>();
        let details = error.details.get_or_insert_with(|| json!({}));
        details["valid_boundaries"] = json!(candidates);
        details["boundary_scope"] = json!("prefix");
        error
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::tests::staged_snapshot;

    #[test]
    fn node_endpoints_are_exact_and_reject_half_tool_groups_with_candidates() {
        let snapshot = staged_snapshot();
        assert_eq!(from_nodes(&snapshot, "a-call", "a-result").unwrap(), 3..4);
        assert_eq!(from_nodes(&snapshot, "last", "a-result").unwrap(), 1..4);
        for (from, to) in [
            ("a-result", "a-result"),
            ("a-call", "a-call"),
            ("latest", "old"),
            ("missing", "old"),
        ] {
            let error = from_nodes(&snapshot, from, to).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument);
            assert!(
                error.details.unwrap()["valid_boundaries"]
                    .as_array()
                    .is_some_and(|values| !values.is_empty())
            );
        }
    }
    #[test]
    fn range_validation_rejects_bounds_overlap_unfinished_calls_and_protected_nodes() {
        let mut snapshot = staged_snapshot();
        for ranges in [
            std::iter::once(0..0).collect(),
            std::iter::once(0..7).collect(),
            vec![0..2, 1..3],
            vec![3..4, 0..1],
        ] {
            assert!(validate(&snapshot, &ranges, RangeMode::Summary).is_err());
        }
        assert!(validate(&snapshot, &[], RangeMode::Spill).is_ok());
        assert!(validate(&snapshot, std::slice::from_ref(&(3..4)), RangeMode::Summary).is_ok());
        snapshot.groups[3].complete = false;
        for mode in [RangeMode::Spill, RangeMode::Summary, RangeMode::Trim] {
            assert!(
                validate(&snapshot, std::slice::from_ref(&(3..4)), mode)
                    .unwrap_err()
                    .message
                    .contains("incomplete or running")
            );
        }
        snapshot.groups[3].complete = true;
        snapshot.protected.insert("a-result".into());
        for mode in [RangeMode::Spill, RangeMode::Summary, RangeMode::Trim] {
            assert!(
                validate(&snapshot, std::slice::from_ref(&(3..4)), mode)
                    .unwrap_err()
                    .message
                    .contains("protected")
            );
        }
    }
    #[test]
    fn explicit_summary_and_spill_fences_differ_from_trim_request_and_latest_protection() {
        let snapshot = staged_snapshot();
        for mode in [RangeMode::Summary, RangeMode::Spill] {
            assert!(validate(&snapshot, std::slice::from_ref(&(5..6)), mode).is_ok());
            assert!(validate(&snapshot, std::slice::from_ref(&(2..3)), mode).is_ok());
        }
        assert!(validate(&snapshot, &[0..2, 3..5], RangeMode::Trim).is_ok());
        assert!(validate(&snapshot, std::slice::from_ref(&(2..3)), RangeMode::Trim).is_err());
        assert!(validate(&snapshot, std::slice::from_ref(&(5..6)), RangeMode::Trim).is_err());
    }
    #[test]
    fn trim_error_candidates_exclude_current_request_and_latest_group() {
        let snapshot = staged_snapshot();
        for rejected in [2..3, 5..6] {
            let error = validate(&snapshot, &[rejected], RangeMode::Trim).unwrap_err();
            let candidates = error.details.unwrap()["valid_boundaries"]
                .as_array()
                .unwrap()
                .clone();
            assert!(!candidates.is_empty());
            for candidate in candidates {
                assert_ne!(candidate["from_node_id"], "current");
                assert_ne!(candidate["from_node_id"], "latest");
                let range = from_nodes(
                    &snapshot,
                    candidate["from_node_id"].as_str().unwrap(),
                    candidate["to_node_id"].as_str().unwrap(),
                )
                .unwrap();
                assert!(validate(&snapshot, &[range], RangeMode::Trim).is_ok());
            }
        }
        let mut only_request = snapshot;
        only_request.groups = vec![only_request.groups[2].clone()];
        let error = validate(
            &only_request,
            std::slice::from_ref(&(0..1)),
            RangeMode::Trim,
        )
        .unwrap_err();
        assert_eq!(error.details.unwrap()["valid_boundaries"], json!([]));
    }
    #[test]
    fn invalid_trim_endpoints_offer_only_ranges_that_can_be_omitted() {
        let snapshot = staged_snapshot();
        for (from, to) in [
            ("missing", "latest"),
            ("a-result", "latest"),
            ("latest", "old"),
        ] {
            let error = from_nodes_for_mode(&snapshot, from, to, RangeMode::Trim).unwrap_err();
            for candidate in error.details.unwrap()["valid_boundaries"]
                .as_array()
                .unwrap()
            {
                let range = from_nodes_for_mode(
                    &snapshot,
                    candidate["from_node_id"].as_str().unwrap(),
                    candidate["to_node_id"].as_str().unwrap(),
                    RangeMode::Trim,
                )
                .unwrap();
                assert!(validate(&snapshot, std::slice::from_ref(&range), RangeMode::Trim).is_ok());
            }
        }
    }

    #[test]
    fn invalid_prefix_trim_candidates_retain_a_fixed_start_and_cross_no_protected_group() {
        let snapshot = staged_snapshot();
        for to in ["missing", "current", "a-result", "latest"] {
            let error = from_prefix_end(&snapshot, to).unwrap_err();
            for candidate in error.details.unwrap()["valid_boundaries"]
                .as_array()
                .unwrap()
            {
                assert_eq!(candidate["from_node_id"], "old");
                let selected =
                    from_prefix_end(&snapshot, candidate["to_node_id"].as_str().unwrap()).unwrap();
                assert_eq!(selected.start, 0);
                assert!(
                    validate(&snapshot, std::slice::from_ref(&selected), RangeMode::Trim).is_ok()
                );
            }
        }
    }
}
