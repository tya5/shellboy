//! API ordering only: persisted source order and DAG edges remain immutable.
use super::*;
use crate::protocol::{ApiError, ErrorCode};
use std::collections::HashSet;

fn invalid() -> StorageError {
    ApiError::new(ErrorCode::InvalidState, "History contains an incomplete or inconsistent tool group")
        .hint("Restart Shellboy to reconcile interrupted tools; inspect interrupted-work details before retrying side effects.")
        .into()
}
fn kind(item: &Value) -> &str {
    item.get("type").and_then(Value::as_str).unwrap_or("")
}
fn call_id(item: &Value) -> Result<&str, StorageError> {
    item.get("call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(invalid)
}

// A spill stores one complete tool group and interleaved context in one node.
// Move converted Values once before caching; do not clone or rewrite its source.
pub(super) fn inline_spill(items: Vec<Value>) -> Result<Vec<Value>, StorageError> {
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

pub(super) fn ordered(frames: Vec<Arc<Frame>>) -> Result<Vec<Arc<Frame>>, StorageError> {
    let mut anchors = HashSet::new();
    for frame in &frames {
        if frame.items.iter().any(|item| kind(item) == "function_call")
            && let Some(group) = &frame.group
            && !anchors.insert(group.as_str())
        {
            return Err(invalid());
        }
    }
    let mut results: HashMap<String, Vec<Arc<Frame>>> = HashMap::new();
    let mut ordinary = Vec::with_capacity(frames.len());
    // Own the group keys, since the original frame vector is consumed below.
    let anchors: HashSet<String> = anchors.into_iter().map(str::to_owned).collect();
    for frame in frames {
        if let Some(group) = &frame.group
            && frame
                .items
                .iter()
                .any(|item| kind(item) == "function_call_output")
        {
            if !anchors.contains(group)
                || frame
                    .items
                    .iter()
                    .any(|item| kind(item) != "function_call_output")
            {
                return Err(invalid());
            }
            results.entry(group.clone()).or_default().push(frame);
        } else {
            ordinary.push(frame);
        }
    }
    let mut ordered =
        Vec::with_capacity(ordinary.len() + results.values().map(Vec::len).sum::<usize>());
    for frame in ordinary {
        let group = frame
            .group
            .as_deref()
            .filter(|_| frame.items.iter().any(|item| kind(item) == "function_call"));
        let outputs = group.and_then(|group| results.remove(group));
        if let Some(outputs) = &outputs {
            let mut pending = HashSet::new();
            for item in &frame.items {
                if kind(item) == "function_call" && !pending.insert(call_id(item)?) {
                    return Err(invalid());
                }
            }
            for output in outputs.iter().flat_map(|frame| &frame.items) {
                if !pending.remove(call_id(output)?) {
                    return Err(invalid());
                }
            }
            if !pending.is_empty() {
                return Err(invalid());
            }
        } else if group.is_some() {
            return Err(invalid());
        }
        ordered.push(frame);
        if let Some(outputs) = outputs {
            ordered.extend(outputs);
        }
    }
    // Also validate ungrouped API fixtures/items, without inventing associations.
    let mut pending = HashSet::new();
    for frame in &ordered {
        if !pending.is_empty() && frame.items.iter().any(|item| kind(item) == "function_call") {
            return Err(invalid());
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn frame(id: &str, group: Option<&str>, items: Vec<Value>, ledger: &Arc<Ledger>) -> Arc<Frame> {
        ledger.live.fetch_add(1, Ordering::AcqRel);
        Arc::new(Frame {
            stamp: Stamp {
                id: id.into(),
                revision: 0,
                tombstone: false,
            },
            group: group.map(str::to_owned),
            items,
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
        let lease = ReplayLease::new(frames).unwrap();
        let sources: Vec<_> = lease.sourced_items().map(|(id, _)| id).collect();
        assert_eq!(
            sources,
            ["c1", "c1", "r2", "r1", "audit", "attachment", "c2", "r3"]
        );
        assert_eq!(original[1].stamp.id, "audit");
        assert!(Arc::ptr_eq(&original[4], &lease.0.frames[2]));
        assert_eq!(ledger.pinned.load(Ordering::Acquire), 7);
        drop(lease);
        assert_eq!(ledger.pinned.load(Ordering::Acquire), 0);
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
                ReplayLease::new(frames),
                Err(StorageError::Operation(_))
            ));
            assert_eq!(ledger.pinned.load(Ordering::Acquire), 0);
            assert_eq!(ledger.live.load(Ordering::Acquire), 0);
        }
        let ledger = Arc::new(Ledger::default());
        let orphan = frame("orphan", Some("other"), vec![output("a")], &ledger);
        assert!(ReplayLease::new(vec![orphan]).is_err());
        assert_eq!(ledger.pinned.load(Ordering::Acquire), 0);
    }
}
