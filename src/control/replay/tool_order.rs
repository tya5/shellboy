//! API ordering only: persisted source order and DAG edges remain immutable.
use super::*;
use crate::protocol::{ApiError, ErrorCode};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy)]
pub(super) struct ToolGroupFault;
impl From<ToolGroupFault> for StorageError {
    fn from(_: ToolGroupFault) -> Self {
        strict_invalid()
    }
}
fn invalid() -> ToolGroupFault {
    ToolGroupFault
}
fn strict_invalid() -> StorageError {
    ApiError::new(ErrorCode::InvalidState, "History contains an incomplete or inconsistent tool group")
        .hint("Inspect saved history; fork at a complete node before the incomplete tool group, or delete the affected Thread. See shellboy docs get --id shellboy/recovery-and-storage.")
        .into()
}
fn kind(item: &Value) -> &str {
    item.get("type").and_then(Value::as_str).unwrap_or("")
}
fn call_id(item: &Value) -> Result<&str, ToolGroupFault> {
    item.get("call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(invalid)
}

// A spill stores one complete tool group and interleaved context in one node.
// Move converted Values once before caching; do not clone or rewrite its source.
pub(super) fn inline_spill_checked(items: Vec<Value>) -> Result<Vec<Value>, ToolGroupFault> {
    let Some(last_call) = items.iter().rposition(|item| kind(item) == "function_call") else {
        return Ok(items);
    };
    let mut pending = HashSet::new();
    for item in &items {
        if kind(item) == "function_call" && !pending.insert(call_id(item)?) {
            return Err(invalid());
        }
    }
    for item in &items {
        if kind(item) == "function_call_output" && !pending.remove(call_id(item)?) {
            return Err(invalid());
        }
    }
    if !pending.is_empty() {
        return Err(invalid());
    }
    drop(pending);
    let mut ordinary = Vec::with_capacity(items.len());
    let mut results = Vec::new();
    let mut insertion = 0;
    for (i, item) in items.into_iter().enumerate() {
        if kind(&item) == "function_call_output" {
            results.push(item);
        } else {
            ordinary.push(item);
        }
        if i == last_call {
            insertion = ordinary.len();
        }
    }
    ordinary.splice(insertion..insertion, results);
    Ok(ordinary)
}

pub(super) struct OrderingPlan {
    anchors: usize,
    results: usize,
    calls: usize,
}

impl OrderingPlan {
    pub(super) fn of(frames: &[Arc<Frame>]) -> Self {
        let mut plan = Self {
            anchors: 0,
            results: 0,
            calls: 0,
        };
        for frame in frames {
            let mut calls = 0;
            let mut output = false;
            for item in &frame.items {
                match kind(item) {
                    "function_call" => calls += 1,
                    "function_call_output" => output = true,
                    _ => {}
                }
            }
            plan.calls = plan.calls.max(calls);
            if frame.group.is_some() {
                plan.anchors += usize::from(calls != 0);
                plan.results += usize::from(output);
            }
        }
        plan
    }

    pub(super) fn bytes(&self, frames: usize) -> usize {
        // One pre-sized result vector, borrowed keys, and one reusable call
        // set. SwissTable stores 7 usable entries per 8 buckets at scale.
        let call_set = if self.calls == 0 {
            0
        } else {
            (self.calls * 8).div_ceil(7).max(4).next_power_of_two() * (size_of::<&str>() + 1) + 32
        };
        frames * size_of::<Arc<Frame>>()
            + self.anchors * size_of::<&str>()
            + self.results * size_of::<(&str, usize)>()
            + call_set
    }

    pub(super) fn order(self, frames: &[Arc<Frame>]) -> Result<Vec<Arc<Frame>>, ToolGroupFault> {
        let mut anchors = Vec::with_capacity(self.anchors);
        let mut results = Vec::with_capacity(self.results);
        for (index, frame) in frames.iter().enumerate() {
            if let Some(group) = frame.group.as_deref() {
                if frame.items.iter().any(|item| kind(item) == "function_call") {
                    anchors.push(group);
                }
                if grouped_result(frame) {
                    if frame
                        .items
                        .iter()
                        .any(|item| kind(item) != "function_call_output")
                    {
                        return Err(invalid());
                    }
                    results.push((group, index));
                }
            }
        }
        anchors.sort_unstable();
        if anchors.windows(2).any(|pair| pair[0] == pair[1])
            || results
                .iter()
                .any(|(group, _)| anchors.binary_search(group).is_err())
        {
            return Err(invalid());
        }
        // Tuple order preserves selected-path order inside each group.
        results.sort_unstable();
        let mut ordered = Vec::with_capacity(frames.len());
        let mut pending = HashSet::with_capacity(self.calls);
        for frame in frames.iter().filter(|frame| !grouped_result(frame)) {
            ordered.push(frame.clone());
            if let Some(group) = frame
                .group
                .as_deref()
                .filter(|_| frame.items.iter().any(|item| kind(item) == "function_call"))
            {
                let start = results.partition_point(|(key, _)| *key < group);
                let end = results.partition_point(|(key, _)| *key <= group);
                pending.clear();
                for item in &frame.items {
                    if kind(item) == "function_call" && !pending.insert(call_id(item)?) {
                        return Err(invalid());
                    }
                }
                for &(_, index) in &results[start..end] {
                    let output = &frames[index];
                    for item in &output.items {
                        if !pending.remove(call_id(item)?) {
                            return Err(invalid());
                        }
                    }
                    ordered.push(output.clone());
                }
                if !pending.is_empty() {
                    return Err(invalid());
                }
            }
        }
        // Also validate ungrouped API fixtures/items, without inventing associations.
        for frame in &ordered {
            if frame.items.iter().any(|item| kind(item) == "function_call") {
                if !pending.is_empty() {
                    return Err(invalid());
                }
                pending.clear();
            }
            for item in &frame.items {
                match kind(item) {
                    "function_call" => {
                        if !pending.insert(call_id(item)?) {
                            return Err(invalid());
                        }
                    }
                    "function_call_output" => {
                        if !pending.remove(call_id(item)?) {
                            return Err(invalid());
                        }
                    }
                    _ if !pending.is_empty()
                        && matches!(
                            item.get("role").and_then(Value::as_str),
                            Some("user" | "developer" | "system")
                        ) =>
                    {
                        return Err(invalid());
                    }
                    _ => {}
                }
            }
        }
        if !pending.is_empty() {
            return Err(invalid());
        }
        Ok(ordered)
    }
}

fn grouped_result(frame: &Frame) -> bool {
    frame.group.is_some()
        && frame
            .items
            .iter()
            .any(|item| kind(item) == "function_call_output")
}

pub(super) fn ordered_checked(frames: &[Arc<Frame>]) -> Result<Vec<Arc<Frame>>, ToolGroupFault> {
    OrderingPlan::of(frames).order(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn frame(id: &str, group: Option<&str>, items: Vec<Value>, ledger: &Arc<Ledger>) -> Arc<Frame> {
        ledger.live.fetch_add(1, Ordering::AcqRel);
        Arc::new(Frame {
            ordinal: 0,
            stamp: Stamp {
                id: id.into(),
                revision: 0,
                tombstone: false,
                kind: "fixture".into(),
                preamble: None,
                imported: false,
            },
            group: group.map(str::to_owned),
            items,
            spill_result_item: false,
            spill_projection_safe: true,
            spill_marker: false,
            bytes: 1,
            pins: AtomicUsize::new(0),
            ledger: ledger.clone(),
        })
    }
    fn call(id: &str) -> Value {
        json!({"type":"function_call","call_id":id,"name":"shell","arguments":"{}"})
    }
    fn output(id: &str) -> Value {
        json!({"type":"function_call_output","call_id":id,"output":"saved"})
    }
    #[test]
    fn reorders_only_result_frames_and_preserves_sources_and_repeated_provider_ids() {
        let ledger = Arc::new(Ledger::default());
        let frames = vec![
            frame(
                "c1",
                Some("g1"),
                vec![call("same"), call("second")],
                &ledger,
            ),
            frame(
                "audit",
                None,
                vec![json!({"role":"developer","content":"audit"})],
                &ledger,
            ),
            frame("r2", Some("g1"), vec![output("second")], &ledger),
            frame(
                "attachment",
                None,
                vec![json!({"role":"user","content":"attachment-source"})],
                &ledger,
            ),
            frame("r1", Some("g1"), vec![output("same")], &ledger),
            frame("c2", Some("g2"), vec![call("same")], &ledger),
            frame("r3", Some("g2"), vec![output("same")], &ledger),
        ];
        let original = frames.clone();
        let lease = ReplayLease::new(&frames, None).unwrap();
        let sources: Vec<_> = lease.sourced_items().map(|(id, _, _)| id).collect();
        assert_eq!(
            sources,
            ["c1", "c1", "r2", "r1", "audit", "attachment", "c2", "r3"]
        );
        let old_framing = original
            .iter()
            .map(|frame| {
                let stamp = &frame.stamp;
                format!(
                    "{}\u{1f}{}\u{1f}{}\u{1e}",
                    stamp.id,
                    stamp.revision,
                    u8::from(stamp.tombstone)
                )
            })
            .collect::<String>();
        assert_eq!(
            lease.witness().sha256,
            crate::digest::sha256_hex(old_framing)
        );
        assert_eq!(original[1].stamp.id, "audit");
        assert!(Arc::ptr_eq(&original[4], &lease.0.frames[2]));
        assert_eq!(ledger.pinned.load(Ordering::Acquire), 7);
        drop(lease);
        assert_eq!(ledger.pinned.load(Ordering::Acquire), 0);
    }
    #[test]
    fn replay_lease_budget_refuses_ordering_before_any_pins() {
        let ledger = Arc::new(Ledger::default());
        let plain = [
            frame("call", None, vec![], &ledger),
            frame("result", None, vec![], &ledger),
        ];
        let grouped = [
            frame("call", Some("group"), vec![call("a")], &ledger),
            frame("result", Some("group"), vec![output("a")], &ledger),
        ];
        // The remaining allowance covers the final Arc, digest and tail, and
        // the ordered pointers, but not the tool-group ordering workspace.
        let remaining = size_of::<LeaseFrames>()
            + 2 * size_of::<usize>()
            + SHA256_HEX_BYTES
            + "result".len()
            + plain.len() * size_of::<Arc<Frame>>();
        drop(ReplayLease::new(&plain, Some(&AssemblyBudget(remaining))).unwrap());
        let Err(error) = ReplayLease::new(&grouped, Some(&AssemblyBudget(remaining))) else {
            panic!("Ordering over budget produced a lease");
        };
        assert_eq!(error.into_api_error().code, ErrorCode::InputLimit);
        assert_eq!(ledger.pinned.load(Ordering::Acquire), 0);
        let lease = ReplayLease::new(&grouped, Some(&AssemblyBudget(MAX_RESPONSE_BYTES))).unwrap();
        assert_eq!(lease.items().count(), 2);
    }

    #[test]
    fn invalid_pairs_never_acquire_pins_or_pull_unselected_results() {
        for (calls, results) in [
            (vec![call("a")], vec![]),
            (vec![call("a"), call("a")], vec![output("a")]),
            (vec![call("a")], vec![output("a"), output("a")]),
            (vec![call("a")], vec![output("different")]),
        ] {
            let ledger = Arc::new(Ledger::default());
            let mut frames = vec![frame("call", Some("group"), calls, &ledger)];
            if !results.is_empty() {
                frames.push(frame("result", Some("group"), results, &ledger));
            }
            assert!(matches!(
                ReplayLease::new(&frames, None),
                Err(StorageError::Operation(_))
            ));
            assert_eq!(ledger.pinned.load(Ordering::Acquire), 0);
            drop(frames);
            assert_eq!(ledger.live.load(Ordering::Acquire), 0);
        }
        let ledger = Arc::new(Ledger::default());
        let orphan = frame("orphan", Some("other"), vec![output("a")], &ledger);
        assert!(ReplayLease::new(&[orphan], None).is_err());
        assert_eq!(ledger.pinned.load(Ordering::Acquire), 0);
    }
}
