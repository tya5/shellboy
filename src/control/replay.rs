//! Immutable Responses items with one byte-bounded LRU shared by all Threads.
use crate::storage::StorageError;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    mem::size_of,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

mod inspection;
mod tool_order;

const ADAPTER_VERSION: u32 = 3;
const TOOL_SCHEMA_VERSION: u32 = 1;
#[derive(Clone, Debug, PartialEq, Eq)]
struct Context {
    thread: String,
    selection: i64,
    profile_revision: i64,
    profile: String,
    model: String,
    epoch: i64,
    adapter_version: u32,
    tool_schema_version: u32,
}
impl Context {
    fn same_conversion(&self, other: &Self) -> bool {
        self.thread == other.thread
            && self.profile == other.profile
            && self.model == other.model
            && self.epoch == other.epoch
            && self.adapter_version == other.adapter_version
            && self.tool_schema_version == other.tool_schema_version
    }
    fn bytes(&self) -> usize {
        self.thread.capacity() + self.profile.capacity() + self.model.capacity()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    id: String,
    revision: i64,
    tombstone: bool,
    kind: String,
}
#[derive(Default)]
struct Ledger {
    live: AtomicUsize,
    pinned: AtomicUsize,
}
struct Frame {
    ordinal: usize,
    stamp: Stamp,
    items: Vec<Value>,
    group: Option<String>,
    spill_result_item: bool,
    spill_projection_safe: bool,
    spill_marker: bool,
    bytes: usize,
    pins: AtomicUsize,
    ledger: Arc<Ledger>,
}
impl Drop for Frame {
    fn drop(&mut self) {
        self.ledger.live.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
struct LeaseFrames {
    frames: Vec<Arc<Frame>>,
    witness: PathWitness,
}
/// Identifies the exact saved path nodes (in path order, with payload
/// revisions and tombstones) a lease was built from. Recorded in an attempt's
/// input manifest so `shellboy thread request --attempt` can tell whether the
/// current saved path still begins with the same nodes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PathWitness {
    pub nodes: usize,
    pub sha256: String,
}
impl PathWitness {
    fn of(frames: &[Arc<Frame>]) -> Self {
        let mut framed = String::new();
        for frame in frames {
            let stamp = &frame.stamp;
            framed.push_str(&format!(
                "{}\u{1f}{}\u{1f}{}\u{1e}",
                stamp.id, stamp.revision, stamp.tombstone as u8
            ));
        }
        Self {
            nodes: frames.len(),
            sha256: crate::digest::sha256_hex(framed),
        }
    }
}
impl Drop for LeaseFrames {
    fn drop(&mut self) {
        for frame in &self.frames {
            if frame.pins.fetch_sub(1, Ordering::AcqRel) == 1 {
                frame.ledger.pinned.fetch_sub(frame.bytes, Ordering::AcqRel);
            }
        }
    }
}
/// Clones share one immutable lease; eviction cannot release an in-flight item.
#[derive(Clone)]
pub struct ReplayLease(Arc<LeaseFrames>);
impl ReplayLease {
    fn new(frames: Vec<Arc<Frame>>) -> Result<Self, StorageError> {
        let witness = PathWitness::of(&frames);
        let frames = tool_order::ordered(frames)?;
        for frame in &frames {
            if frame.pins.fetch_add(1, Ordering::AcqRel) == 0 {
                frame.ledger.pinned.fetch_add(frame.bytes, Ordering::AcqRel);
            }
        }
        Ok(Self(Arc::new(LeaseFrames { frames, witness })))
    }
    pub fn witness(&self) -> &PathWitness {
        &self.0.witness
    }
    /// Recover source order without sorting or copying any model payload.
    fn source_order(&self) -> Option<Vec<Option<&Frame>>> {
        let mut ordered = vec![None; self.0.frames.len()];
        for frame in &self.0.frames {
            let slot = ordered.get_mut(frame.ordinal)?;
            if slot.replace(frame.as_ref()).is_some() {
                return None;
            }
        }
        Some(ordered)
    }
    /// Detect changes to an earlier selected path without copying payloads.
    pub(crate) fn extends(&self, previous: &PathWitness) -> bool {
        use sha2::{Digest, Sha256};
        if previous.nodes > self.0.frames.len() {
            return false;
        }
        let Some(ordered) = self.source_order() else {
            return false;
        };
        let mut hash = Sha256::new();
        for frame in ordered.into_iter().take(previous.nodes) {
            let Some(frame) = frame else {
                return false;
            };
            let stamp = &frame.stamp;
            hash.update(stamp.id.as_bytes());
            hash.update([0x1f]);
            hash.update(stamp.revision.to_string().as_bytes());
            hash.update([0x1f, b'0' + stamp.tombstone as u8, 0x1e]);
        }
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            == previous.sha256
    }
    /// Prove that no older tool span can shrink, using a conservative lower
    /// bound for each replacement and at most 8 MiB of already-replayed JSON.
    /// False means "unknown or possibly reducible", never "reducible".
    pub(crate) fn proves_no_reducing_older_tools(&self) -> bool {
        const MAX_BYTES: usize = 8 * 1024 * 1024;
        const MAX_FRAMES: usize = 4096;
        let mut first = None;
        let mut multiple = false;
        for frame in &self.0.frames {
            if let Some(group) = frame.group.as_deref() {
                match first {
                    None => first = Some(group),
                    Some(previous) if previous != group => {
                        multiple = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
        if !multiple {
            return true;
        }
        if self.0.frames.len() > MAX_FRAMES {
            return false;
        }
        let Some(ordered) = self.source_order() else {
            return false;
        };
        let frames = ordered.into_iter().collect::<Option<Vec<_>>>();
        let Some(frames) = frames else {
            return false;
        };
        let mut last = HashMap::new();
        for (index, frame) in frames.iter().enumerate() {
            if let Some(group) = frame.group.as_deref() {
                last.insert(group, index);
            }
        }
        let mut spans = Vec::new();
        let mut start = 0;
        let mut end = 0;
        let mut has_group = false;
        for (index, frame) in frames.iter().enumerate() {
            if let Some(group) = frame.group.as_deref() {
                end = end.max(last[group]);
                has_group = true;
            }
            if index == end {
                if has_group {
                    spans.push(start..index + 1);
                }
                start = index + 1;
                end = index + 1;
                has_group = false;
            }
        }
        // Considering every older span, even one that is incomplete or
        // protected, is conservative: it can only make this proof fail.
        if spans.len() <= 1 {
            return true;
        }
        let mut remaining = MAX_BYTES;
        for span in &spans[..spans.len() - 1] {
            let mut before = 0u64;
            let mut after_lower = 0u64;
            for frame in &frames[span.clone()] {
                if !frame.spill_projection_safe {
                    return false;
                }
                for item in &frame.items {
                    let Some(chars) = bounded_json_characters(item, &mut remaining) else {
                        return false;
                    };
                    before = before.saturating_add(chars.saturating_add(1));
                    if item.get("type").and_then(Value::as_str) == Some("reasoning")
                        || crate::model::opaque::is_opaque(item)
                    {
                        continue;
                    }
                    if frame.spill_result_item
                        && item.get("type").and_then(Value::as_str) == Some("function_call_output")
                    {
                        let call = item.get("call_id").and_then(Value::as_str).unwrap_or("");
                        let minimum = json!({"omitted":true,"call_id":call,"output_bytes":0,"status":0,"execution_id":0,"exit_code":0,"original_node_id":frame.stamp.id,"retrieve":"shellboy history get --node <original_node_id>"});
                        let replacement = json!({"type":"function_call_output","call_id":call,"output":minimum.to_string()});
                        let Some(chars) = bounded_json_characters(&replacement, &mut remaining)
                        else {
                            return false;
                        };
                        after_lower = after_lower.saturating_add(chars.saturating_add(1));
                    } else {
                        after_lower = after_lower.saturating_add(chars.saturating_add(1));
                    }
                }
            }
            if after_lower < before {
                return false;
            }
        }
        true
    }

    /// Node anchoring the newest replayable tool group, if one exists.
    pub(crate) fn latest_tool_anchor(&self) -> Option<&str> {
        self.0
            .frames
            .iter()
            .filter(|frame| {
                frame.group.is_some()
                    && frame.items.iter().any(|item| {
                        item.get("type").and_then(Value::as_str) == Some("function_call")
                    })
            })
            .max_by_key(|frame| frame.ordinal)
            .map(|frame| frame.stamp.id.as_str())
    }

    /// Active-Turn summary can cover an older tool group or spill marker even
    /// if that group itself is not worth spilling. Return true only when that
    /// scope is structurally impossible, without inspecting user content.
    pub(crate) fn proves_no_active_middle(&self) -> bool {
        let mut first = None;
        for frame in &self.0.frames {
            if frame.spill_marker {
                return false;
            }
            if let Some(group) = frame.group.as_deref() {
                match first {
                    None => first = Some(group),
                    Some(previous) if previous != group => return false,
                    _ => {}
                }
            }
        }
        true
    }

    /// Only completed groups before the newest can become ordinary spill
    /// targets. Ignore growth of the newest, protected group.
    pub(crate) fn old_tool_frontier(&self) -> String {
        use sha2::{Digest, Sha256};
        let Some(ordered) = self.source_order() else {
            return crate::digest::sha256_hex(format!("invalid:{}", self.0.witness.sha256));
        };
        let mut groups = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for frame in ordered.iter().flatten().copied() {
            if let Some(group) = frame.group.as_deref()
                && frame
                    .items
                    .iter()
                    .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
                && seen.insert(group)
            {
                groups.push(group);
            }
        }
        let Some(latest) = groups.pop() else {
            return crate::digest::sha256_hex("");
        };
        let prior = groups.into_iter().collect::<std::collections::HashSet<_>>();
        let mut hash = Sha256::new();
        for frame in ordered.into_iter().flatten() {
            if let Some(group) = frame.group.as_deref()
                && group != latest
                && prior.contains(group)
            {
                let stamp = &frame.stamp;
                hash.update(group.as_bytes());
                hash.update([0x1f]);
                hash.update(stamp.id.as_bytes());
                hash.update([0x1f]);
                hash.update(stamp.revision.to_string().as_bytes());
                hash.update([0x1f, b'0' + stamp.tombstone as u8, 0x1e]);
            }
        }
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    }
    pub fn items(&self) -> impl Iterator<Item = &Value> {
        self.0.frames.iter().flat_map(|frame| &frame.items)
    }
    /// Keep provenance with cached conversion; variable attachment bytes are
    /// bound after this immutable text plan has been loaded. The category is
    /// the item's source for request byte accounting (#186), local-only.
    pub(crate) fn sourced_items(
        &self,
    ) -> impl Iterator<Item = (&str, crate::model::wire_bytes::Category, &Value)> {
        self.0.frames.iter().flat_map(|frame| {
            frame.items.iter().map(|item| {
                (
                    frame.stamp.id.as_str(),
                    crate::model::wire_bytes::Category::of_history(
                        &frame.stamp.kind,
                        frame.spill_marker,
                        item,
                    ),
                    item,
                )
            })
        })
    }
    pub fn materialize(&self) -> Vec<Value> {
        self.items().cloned().collect()
    }
}
struct Entry {
    context: Context,
    frames: Vec<Arc<Frame>>,
    bytes: usize,
    overhead: usize,
}
#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    order: VecDeque<String>,
    retained: usize,
    hits: u64,
    append_hits: u64,
    rebuilds: u64,
    parsed_nodes: u64,
    evictions: u64,
    settings_reuses: u64,
}
impl State {
    fn metadata_bytes(&self) -> usize {
        let capacity = self.entries.capacity();
        // HashMap::capacity is usable slots; account for SwissTable's spare
        // buckets/control group as well as the VecDeque's allocated capacity.
        let buckets = if capacity == 0 {
            0
        } else {
            (capacity.saturating_mul(8) / 7).next_power_of_two()
        };
        let map = if buckets == 0 {
            0
        } else {
            buckets
                .saturating_mul(size_of::<(String, Entry)>() + 1)
                .saturating_add(32)
        };
        map.saturating_add(self.order.capacity() * size_of::<String>())
    }
    fn retained_bytes(&self) -> usize {
        self.retained.saturating_add(self.metadata_bytes())
    }
    fn compact_metadata(&mut self) {
        if self.entries.is_empty() {
            self.entries = HashMap::new();
            self.order = VecDeque::new();
        } else if self.entries.capacity() > self.entries.len().saturating_mul(4).max(16) {
            self.entries.shrink_to_fit();
            self.order.shrink_to_fit();
        }
    }
}
/// Byte counts include conservative structure costs, not only JSON wire length.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ReplayStats {
    pub retained_bytes: usize,
    pub live_bytes: usize,
    pub pinned_bytes: usize,
    pub threads: usize,
    pub metadata_bytes: usize,
    pub hits: u64,
    pub append_hits: u64,
    pub rebuilds: u64,
    pub parsed_nodes: u64,
    pub evictions: u64,
    pub settings_reuses: u64,
}
#[derive(Clone)]
pub struct ReplayCache {
    budget: usize,
    state: Arc<Mutex<State>>,
    ledger: Arc<Ledger>,
}
impl ReplayCache {
    pub fn new(budget: u64) -> Self {
        Self {
            budget: usize::try_from(budget).unwrap_or(usize::MAX),
            state: Arc::new(Mutex::new(State::default())),
            ledger: Arc::new(Ledger::default()),
        }
    }
    pub fn stats(&self) -> ReplayStats {
        let state = self.state.lock().expect("replay cache poisoned");
        ReplayStats {
            retained_bytes: state.retained_bytes(),
            live_bytes: self.ledger.live.load(Ordering::Acquire).saturating_add(
                state
                    .entries
                    .values()
                    .map(|entry| entry.overhead)
                    .sum::<usize>()
                    .saturating_add(state.metadata_bytes()),
            ),
            pinned_bytes: self.ledger.pinned.load(Ordering::Acquire),
            threads: state.entries.len(),
            metadata_bytes: state.metadata_bytes(),
            hits: state.hits,
            append_hits: state.append_hits,
            rebuilds: state.rebuilds,
            parsed_nodes: state.parsed_nodes,
            evictions: state.evictions,
            settings_reuses: state.settings_reuses,
        }
    }
    /// Call in the DB snapshot transaction. Metadata is always revalidated; only
    /// an unchanged prefix can reuse items, and only the new suffix is decoded.
    pub fn load(&self, db: &Connection, thread: &str) -> Result<ReplayLease, StorageError> {
        let context=db.query_row("SELECT selection_revision,profile_revision,profile_id,model_id,reasoning_epoch FROM thread WHERE id=?1 AND deleted_at IS NULL",[thread],|row|Ok(Context{thread:thread.into(),selection:row.get(0)?,profile_revision:row.get(1)?,profile:row.get(2)?,model:row.get(3)?,epoch:row.get(4)?,adapter_version:ADAPTER_VERSION,tool_schema_version:TOOL_SCHEMA_VERSION}))?;
        // Bound to `context.epoch`, so the epoch check below also covers it.
        let keep = crate::history::reasoning_keep(db, thread, context.epoch)?;
        let stamps=db.prepare("SELECT n.id,n.payload_revision,n.deleted_reason IS NOT NULL OR n.payload_json IS NULL,n.kind FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 ORDER BY p.ordinal")?.query_map([thread],|row|Ok(Stamp{id:row.get(0)?,revision:row.get(1)?,tombstone:row.get(2)?,kind:row.get(3)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut state = self.state.lock().expect("replay cache poisoned");
        let previous = state.entries.remove(thread);
        state.order.retain(|id| id != thread);
        if let Some(previous) = &previous {
            state.retained = state.retained.saturating_sub(previous.bytes);
        }
        let prefix = previous.as_ref().filter(|entry| {
            entry.context.same_conversion(&context)
                && entry.frames.len() <= stamps.len()
                && entry
                    .frames
                    .iter()
                    .zip(&stamps)
                    .all(|(frame, stamp)| frame.stamp == *stamp)
        });
        let reuse = prefix.filter(|entry| {
            entry.frames.len() < stamps.len() || entry.context.selection == context.selection
        });
        let reused = reuse.map_or(0, |entry| entry.frames.len());
        if reuse.is_some_and(|entry| entry.context.profile_revision != context.profile_revision) {
            state.settings_reuses += 1;
        }
        if reused == stamps.len() && reuse.is_some() {
            state.hits += 1;
        } else if reused > 0 {
            state.append_hits += 1;
        } else {
            state.rebuilds += 1;
        }
        let mut frames = Vec::with_capacity(stamps.len());
        if let Some(previous) = reuse {
            frames.extend(previous.frames.iter().cloned());
        }
        if reused < stamps.len() {
            let mut statement=db.prepare("SELECT n.id,n.payload_json FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal>=?2 ORDER BY p.ordinal")?;
            let mut rows = statement.query(rusqlite::params![thread, reused as i64])?;
            let mut offset = reused;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let stamp = stamps
                    .get(offset)
                    .filter(|stamp| stamp.id == id)
                    .ok_or_else(|| {
                        StorageError::Schema("replay path changed within its DB snapshot".into())
                    })?
                    .clone();
                if !stamp.tombstone {
                    state.parsed_nodes += 1;
                }
                let body: Option<String> = if stamp.tombstone {
                    None
                } else {
                    Some(row.get(1)?)
                };
                frames.push(Arc::new(decode_frame(
                    offset,
                    stamp,
                    body,
                    context.epoch,
                    &keep,
                    &self.ledger,
                )?));
                offset += 1;
            }
            if offset != stamps.len() {
                return Err(StorageError::Schema(
                    "replay path row count changed within its DB snapshot".into(),
                ));
            }
        }
        let overhead =
            context.bytes() + frames.capacity() * size_of::<Arc<Frame>>() + thread.len() * 2;
        let bytes = overhead.saturating_add(frames.iter().map(|frame| frame.bytes).sum::<usize>());
        let lease = ReplayLease::new(frames.clone())?;
        drop(previous);
        if self.budget > 0 && bytes <= self.budget {
            state.retained = state.retained.saturating_add(bytes);
            state.order.push_back(thread.into());
            state.entries.insert(
                thread.into(),
                Entry {
                    context,
                    frames,
                    bytes,
                    overhead,
                },
            );
            while state.retained_bytes() > self.budget {
                let Some(old) = state.order.pop_front() else {
                    break;
                };
                if let Some(entry) = state.entries.remove(&old) {
                    state.retained = state.retained.saturating_sub(entry.bytes);
                    state.evictions += 1;
                }
                state.compact_metadata();
            }
        }
        state.compact_metadata();
        Ok(lease)
    }
}
pub(crate) fn convert(value: Value, epoch: i64) -> Vec<Value> {
    convert_kept(value, epoch, false)
}
/// `kept`: the node lies before an earlier edit point and its reasoning was
/// sendable then, so its prefix is unchanged and it replays as saved (#182).
pub(crate) fn convert_kept(mut value: Value, epoch: i64, kept: bool) -> Vec<Value> {
    let stored_epoch = value.get("reasoning_epoch").and_then(Value::as_i64);
    if let Some(output) = value.get_mut("output").and_then(Value::as_array_mut) {
        return std::mem::take(output)
            .into_iter()
            .filter(|item| {
                // Providers bind reasoning and signed opaque blocks to the
                // model; resend them only while the epoch is unchanged.
                (item.get("type").and_then(Value::as_str) != Some("reasoning")
                    && !crate::model::opaque::is_opaque(item))
                    || stored_epoch == Some(epoch)
                    || kept
            })
            .map(outbound_item)
            .collect();
    }
    if let Some(object) = value.as_object_mut() {
        if let Some(item) = object.remove("item") {
            return vec![outbound_item(item)];
        }
        if object.contains_key("role") {
            object.remove("event_id");
            return vec![outbound_item(value)];
        }
    }
    Vec::new()
}
// Provider-local item identities are not conversation identities. Normalize only
// the outbound copy; call_id still pairs calls/results and reasoning is opaque.
fn outbound_item(mut item: Value) -> Value {
    // Generic assistant nodes (for example a saved human-approval ask) may
    // store their text using the user-side input_text content tag. Responses
    // accepts output_text/refusal for assistant messages; normalize only the
    // outbound replay copy, never the saved node or user content.
    if item.get("role").and_then(Value::as_str) == Some("assistant")
        && let Some(content) = item.get_mut("content").and_then(Value::as_array_mut)
    {
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("input_text") {
                part["type"] = Value::String("output_text".into());
            }
        }
    }
    let message_or_call = match item.get("type").and_then(Value::as_str) {
        Some("message" | "function_call") => true,
        None => item.get("role").is_some(),
        _ => false,
    };
    if message_or_call && let Some(object) = item.as_object_mut() {
        object.remove("id");
    }
    item
}
fn bounded_json_characters(item: &Value, remaining: &mut usize) -> Option<u64> {
    struct Counter<'a> {
        remaining: &'a mut usize,
        characters: u64,
    }
    impl std::io::Write for Counter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > *self.remaining {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            *self.remaining -= bytes.len();
            self.characters = self
                .characters
                .saturating_add(bytes.iter().filter(|byte| **byte & 0xc0 != 0x80).count() as u64);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter {
        remaining,
        characters: 0,
    };
    serde_json::to_writer(&mut counter, item).ok()?;
    Some(counter.characters)
}

enum FrameError {
    Storage(StorageError),
    Tool(tool_order::ToolGroupFault),
}
impl From<StorageError> for FrameError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
impl From<tool_order::ToolGroupFault> for FrameError {
    fn from(error: tool_order::ToolGroupFault) -> Self {
        Self::Tool(error)
    }
}
impl From<FrameError> for StorageError {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::Storage(error) => error,
            FrameError::Tool(error) => error.into(),
        }
    }
}
fn decode_frame(
    ordinal: usize,
    stamp: Stamp,
    body: Option<String>,
    epoch: i64,
    keep: &std::collections::HashSet<String>,
    ledger: &Arc<Ledger>,
) -> Result<Frame, StorageError> {
    decode_frame_checked(ordinal, stamp, body, epoch, keep, ledger).map_err(StorageError::from)
}
fn decode_frame_checked(
    ordinal: usize,
    stamp: Stamp,
    body: Option<String>,
    epoch: i64,
    keep: &std::collections::HashSet<String>,
    ledger: &Arc<Ledger>,
) -> Result<Frame, FrameError> {
    let (group, items, spill_result_item, spill_projection_safe, spill_marker) = match body {
        None => (None, Vec::new(), false, true, false),
        Some(body) => {
            let value: Value = serde_json::from_str(&body)
                .map_err(|error| StorageError::Schema(error.to_string()))?;
            let group = value
                .get("group_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let spill = value.get("spill_version").is_some();
            let no_output = value.get("output").and_then(Value::as_array).is_none();
            // convert() prefers role over item, while spill_payload() prefers
            // item over role; never prove a lower bound for that ambiguity.
            let spill_projection_safe =
                !(no_output && value.get("role").is_some() && value.get("item").is_some());
            let spill_result_item = spill_projection_safe
                && no_output
                && value
                    .get("item")
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    == Some("function_call_output");
            let items = convert_kept(value, epoch, keep.contains(&stamp.id));
            (
                group,
                if spill {
                    tool_order::inline_spill_checked(items)?
                } else {
                    items
                },
                spill_result_item,
                spill_projection_safe,
                spill,
            )
        }
    };
    let bytes = size_of::<Frame>()
        + 2 * size_of::<usize>()
        + stamp.id.capacity()
        + stamp.kind.capacity()
        + group.as_ref().map_or(0, String::capacity)
        + items.capacity() * size_of::<Value>()
        + items.iter().map(heap_bytes).sum::<usize>();
    ledger.live.fetch_add(bytes, Ordering::AcqRel);
    Ok(Frame {
        ordinal,
        stamp,
        items,
        group,
        spill_result_item,
        spill_projection_safe,
        spill_marker,
        bytes,
        pins: AtomicUsize::new(0),
        ledger: ledger.clone(),
    })
}

/// Uncached read of the first `nodes` saved path nodes (all when `None`),
/// converted for `epoch`. For read-only inspection; it never touches a cache
/// and reads only the Thread's own selected path.
#[cfg(test)]
pub(crate) fn load_uncached(
    db: &Connection,
    thread: &str,
    nodes: Option<usize>,
    epoch: i64,
) -> Result<ReplayLease, StorageError> {
    load_uncached_mode(db, thread, nodes, epoch, false)
}
pub(crate) fn load_for_inspection(
    db: &Connection,
    thread: &str,
    expected: Option<&PathWitness>,
    epoch: i64,
) -> Result<Option<ReplayLease>, StorageError> {
    if let Some(expected) = expected {
        // Check association before interpreting a different path as this attempt.
        // The caller holds the same read transaction for this and the body read.
        let mut statement = db.prepare("SELECT n.id,n.payload_revision,n.deleted_reason IS NOT NULL OR n.payload_json IS NULL FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 ORDER BY p.ordinal LIMIT ?2")?;
        let mut rows = statement.query(rusqlite::params![
            thread,
            i64::try_from(expected.nodes).unwrap_or(i64::MAX)
        ])?;
        let mut framed = String::new();
        let mut count = 0;
        while let Some(row) = rows.next()? {
            framed.push_str(&format!(
                "{}\u{1f}{}\u{1f}{}\u{1e}",
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, bool>(2)?
            ));
            count += 1;
        }
        if count != expected.nodes || crate::digest::sha256_hex(framed) != expected.sha256 {
            return Ok(None);
        }
    }
    load_uncached_mode(db, thread, expected.map(|w| w.nodes), epoch, true).map(Some)
}
fn load_uncached_mode(
    db: &Connection,
    thread: &str,
    nodes: Option<usize>,
    epoch: i64,
    inspect: bool,
) -> Result<ReplayLease, StorageError> {
    let limit = nodes.map_or(-1, |n| i64::try_from(n).unwrap_or(i64::MAX));
    let ledger = Arc::new(Ledger::default());
    let keep = crate::history::reasoning_keep(db, thread, epoch)?;
    let mut statement = db.prepare("SELECT n.id,n.payload_revision,n.deleted_reason IS NOT NULL OR n.payload_json IS NULL,n.payload_json,n.kind FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 ORDER BY p.ordinal LIMIT ?2")?;
    let mut rows = statement.query(rusqlite::params![thread, limit])?;
    let mut frames = Vec::new();
    let mut decode_fault = false;
    while let Some(row) = rows.next()? {
        let stamp = Stamp {
            id: row.get(0)?,
            revision: row.get(1)?,
            tombstone: row.get(2)?,
            kind: row.get(4)?,
        };
        let body: Option<String> = if stamp.tombstone { None } else { row.get(3)? };
        match decode_frame_checked(frames.len(), stamp, body, epoch, &keep, &ledger) {
            Ok(frame) => frames.push(Arc::new(frame)),
            Err(FrameError::Tool(_)) if inspect => decode_fault = true,
            Err(error) => return Err(error.into()),
        }
    }
    if inspect {
        // Finish decoding the selected snapshot before reporting a tool fault.
        if decode_fault {
            return Err(inspection::unassociated_fault());
        }
        inspection::validate(db, thread, &frames)?;
        tool_order::ordered_checked(frames.clone())
            .map_err(|_| inspection::unassociated_fault())?;
    }
    ReplayLease::new(frames)
}
// serde_json::Value owns nested strings, Vec capacity and map allocations. The
// per-map-entry allowance conservatively covers tree-node pointers/alignment.
fn heap_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.capacity(),
        Value::Array(values) => {
            values.capacity() * size_of::<Value>() + values.iter().map(heap_bytes).sum::<usize>()
        }
        Value::Object(values) => {
            values.len() * (size_of::<(String, Value)>() + size_of::<[usize; 8]>())
                + values
                    .iter()
                    .map(|(key, value)| key.capacity() + heap_bytes(value))
                    .sum::<usize>()
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn proactive_witness_ignores_current_suffix_and_tracks_older_tool_groups() {
        fn frame(ordinal: usize, id: &str, group: Option<&str>, item: Value) -> Arc<Frame> {
            let ledger = Arc::new(Ledger::default());
            ledger.live.fetch_add(1, Ordering::AcqRel);
            let spill_result_item =
                item.get("type").and_then(Value::as_str) == Some("function_call_output");
            Arc::new(Frame {
                ordinal,
                stamp: Stamp {
                    id: id.into(),
                    revision: 1,
                    tombstone: false,
                    kind: "fixture".into(),
                },
                items: vec![item],
                group: group.map(str::to_owned),
                spill_result_item,
                spill_projection_safe: true,
                spill_marker: false,
                bytes: 1,
                pins: AtomicUsize::new(0),
                ledger,
            })
        }
        let call =
            |id: &str| json!({"type":"function_call","call_id":id,"name":"shell","arguments":"{}"});
        let output =
            |id: &str| json!({"type":"function_call_output","call_id":id,"output":"synthetic"});
        let first = vec![
            frame(0, "c1", Some("g1"), call("a")),
            frame(1, "r1", Some("g1"), output("a")),
        ];
        let first_lease = ReplayLease::new(first.clone()).unwrap();
        let witness = first_lease.witness().clone();
        let protected = first_lease.old_tool_frontier();
        let mut appended = first.clone();
        appended.push(frame(
            2,
            "current",
            None,
            json!({"type":"message","role":"assistant","content":[]}),
        ));
        let appended_lease = ReplayLease::new(appended.clone()).unwrap();
        assert!(appended_lease.extends(&witness));
        assert_eq!(
            appended_lease.old_tool_frontier(),
            protected,
            "ordinary current-turn growth is not a new spill target"
        );
        appended.push(frame(3, "c2", Some("g2"), call("b")));
        appended.push(frame(4, "r2", Some("g2"), output("b")));
        let later = ReplayLease::new(appended.clone()).unwrap();
        assert!(
            later.proves_no_reducing_older_tools(),
            "tiny old groups are not reducible"
        );
        assert!(later.extends(&witness));
        assert_ne!(
            later.old_tool_frontier(),
            protected,
            "the first group becomes older than the protected newest group"
        );
        appended[4] = frame(4, "r2-new", Some("g2"), output("b"));
        let latest_changed = ReplayLease::new(appended).unwrap();
        assert_eq!(
            latest_changed.old_tool_frontier(),
            later.old_tool_frontier(),
            "changes to the latest protected group alone do not create a new target"
        );
        let large = ReplayLease::new(vec![
            frame(0,"large-c",Some("big"),call("big-call")),
            frame(1,"large-r",Some("big"),json!({"type":"function_call_output","call_id":"big-call","output":"x".repeat(50_000)})),
            frame(2,"new-c",Some("new"),call("new-call")),
            frame(3,"new-r",Some("new"),output("new-call")),
        ]).unwrap();
        assert!(
            !large.proves_no_reducing_older_tools(),
            "a potentially useful older group is never suppressed"
        );
        let mut ambiguous = frame(
            1,
            "ambiguous-result",
            Some("ambiguous"),
            output("ambiguous-call"),
        );
        Arc::get_mut(&mut ambiguous).unwrap().spill_projection_safe = false;
        let ambiguous = ReplayLease::new(vec![
            frame(
                0,
                "ambiguous-call",
                Some("ambiguous"),
                call("ambiguous-call"),
            ),
            ambiguous,
            frame(2, "last-call", Some("last"), call("last-call")),
            frame(3, "last-result", Some("last"), output("last-call")),
        ])
        .unwrap();
        assert!(
            !ambiguous.proves_no_reducing_older_tools(),
            "ambiguous role/item conversion is never used to prove no target"
        );
        let mut unicode_budget = 4096;
        let unicode = json!({"text":"旧\n🦀\u{1f}é"});
        assert_eq!(
            bounded_json_characters(&unicode, &mut unicode_budget),
            Some(unicode.to_string().chars().count() as u64)
        );
        assert!(bounded_json_characters(&unicode, &mut 1usize).is_none());
        let long = (0..4097)
            .map(|index| {
                frame(
                    index,
                    &format!("long-{index}"),
                    None,
                    json!({"role":"user","content":"synthetic"}),
                )
            })
            .collect();
        assert!(
            ReplayLease::new(long)
                .unwrap()
                .proves_no_reducing_older_tools(),
            "a long protected Turn with no tools needs no group scan"
        );
        let rewritten = ReplayLease::new(vec![frame(
            0,
            "changed",
            None,
            json!({"role":"user","content":"synthetic"}),
        )])
        .unwrap();
        assert!(!rewritten.extends(&witness));
    }

    #[test]
    fn nonreducing_proof_never_hides_a_real_reducing_spill() {
        let ledger = Arc::new(Ledger::default());
        for size in [0, 1, 16, 128, 512, 4096, 50_000] {
            for status in [Value::Null, json!(0), json!(""), json!("completed")] {
                let call = json!({"group_id":"g1","reasoning_epoch":0,"output":[{"type":"function_call","call_id":"a","name":"shell","arguments":"{}"}]});
                let output = json!({"group_id":"g1","item":{"type":"function_call_output","call_id":"a","output":json!({"status":status,"stdout":"x".repeat(size)}).to_string()}});
                let later_call = json!({"group_id":"g2","output":[{"type":"function_call","call_id":"b","name":"shell","arguments":"{}"}]});
                let later_output = json!({"group_id":"g2","item":{"type":"function_call_output","call_id":"b","output":"{}"}});
                let sources = [
                    ("call".to_owned(), call),
                    ("result".to_owned(), output),
                    ("later-call".to_owned(), later_call),
                    ("later-result".to_owned(), later_output),
                ];
                let actual = crate::compaction::test_spill_reduces(&sources[..2], 0);
                let frames = sources
                    .iter()
                    .enumerate()
                    .map(|(ordinal, (id, payload))| {
                        Arc::new(
                            decode_frame(
                                ordinal,
                                Stamp {
                                    id: id.clone(),
                                    revision: 0,
                                    tombstone: false,
                                    kind: "fixture".into(),
                                },
                                Some(payload.to_string()),
                                0,
                                &Default::default(),
                                &ledger,
                            )
                            .unwrap(),
                        )
                    })
                    .collect();
                let replay = ReplayLease::new(frames).unwrap();
                assert!(
                    !(actual && replay.proves_no_reducing_older_tools()),
                    "size={size} actual reducing spill must not be suppressed"
                );
            }
        }
        let sources = [
            (
                "combined-call".to_owned(),
                json!({"group_id":"g1","output":[{"type":"function_call","call_id":"a","name":"shell","arguments":"{}"}]}),
            ),
            (
                "combined-result".to_owned(),
                json!({"group_id":"g1","output":[{"type":"function_call_output","call_id":"a","output":""}]}),
            ),
            (
                "later-call".to_owned(),
                json!({"group_id":"g2","output":[{"type":"function_call","call_id":"b","name":"shell","arguments":"{}"}]}),
            ),
            (
                "later-result".to_owned(),
                json!({"group_id":"g2","item":{"type":"function_call_output","call_id":"b","output":""}}),
            ),
        ];
        assert!(!crate::compaction::test_spill_reduces(&sources[..2], 0));
        let frames = sources
            .iter()
            .enumerate()
            .map(|(ordinal, (id, payload))| {
                Arc::new(
                    decode_frame(
                        ordinal,
                        Stamp {
                            id: id.clone(),
                            revision: 0,
                            tombstone: false,
                            kind: "fixture".into(),
                        },
                        Some(payload.to_string()),
                        0,
                        &Default::default(),
                        &ledger,
                    )
                    .unwrap(),
                )
            })
            .collect();
        let replay = ReplayLease::new(frames).unwrap();
        assert!(
            replay.proves_no_reducing_older_tools(),
            "an unchanged output-array group has a valid proof"
        );
        drop(replay);
        assert_eq!(ledger.live.load(Ordering::Acquire), 0);
    }

    #[test]
    fn opaque_items_replay_byte_exact_only_in_their_epoch() {
        let block = json!({"type":"thinking","thinking":"考え\n\"x\"","signature":"EqQB/+=="});
        let item = crate::model::opaque::wrap("anthropic_messages", "thinking", block.clone());
        let saved = json!({"reasoning_epoch":2,"output":[item,
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]});
        // Through the stored form, as the history reload does.
        let saved: Value = serde_json::from_str(&saved.to_string()).unwrap();
        let same = convert(saved.clone(), 2);
        assert_eq!(same.len(), 2);
        assert_eq!(same[0]["block"], block);
        assert_eq!(same[0]["block"]["signature"], "EqQB/+==");
        // A model or connection change bumps the epoch: the item stays saved
        // but is not sent again; other items are unaffected.
        let later = convert(saved, 3);
        assert_eq!(later.len(), 1);
        assert_eq!(later[0]["role"], "assistant");
    }

    #[test]
    fn replayed_assistant_input_text_is_normalized_for_responses_without_touching_user_text() {
        use crate::config::Api;

        let assistant = json!({"role":"assistant","content":[
            {"type":"input_text","text":"approval question"},
            {"type":"output_text","text":"existing answer"},
            {"type":"refusal","refusal":"refused"}
        ]});
        let user = json!({"role":"user","content":[{"type":"input_text","text":"reply"}]});
        let assistant_replay = convert(assistant.clone(), 0);
        let user_replay = convert(user.clone(), 0);
        let request = crate::model::response_request(
            "synthetic",
            [user_replay, assistant_replay].concat(),
            None,
            false,
        );
        let wire = crate::model::dialect::encode(Api::Responses, request).unwrap();

        assert_eq!(wire["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(wire["input"][1]["content"][0]["type"], "output_text");
        assert_eq!(wire["input"][1]["content"][1]["type"], "output_text");
        assert_eq!(wire["input"][1]["content"][2]["type"], "refusal");
        for part in wire["input"][1]["content"].as_array().unwrap() {
            assert!(matches!(
                part["type"].as_str(),
                Some("output_text" | "refusal")
            ));
        }
        assert_eq!(
            assistant["content"][0]["type"], "input_text",
            "saved history is immutable"
        );
        assert_eq!(user["content"][0]["type"], "input_text");
    }

    #[tokio::test]
    async fn old_adapter_cache_is_rebuilt_from_unchanged_database() {
        let root = tempfile::tempdir().unwrap();
        let worker = crate::storage::DbWorker::open(root.path().join("db"))
            .await
            .unwrap();
        worker.call(|db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/AGENTS.md','p','m','now')", [])?;
            let tx = db.transaction()?;
            crate::history::append_node(&tx, "t", "n", None, "fixture", &json!({"output":[{"type":"message","id":"chatcmpl-old","role":"assistant","content":[],"status":"completed"}]}), "now")?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let cache = ReplayCache::new(1_000_000);
        let run = cache.clone();
        worker.call(move |db| {
            drop(run.load(db, "t")?);
            {
                let mut state = run.state.lock().unwrap();
                let entry = state.entries.get_mut("t").unwrap();
                entry.context.adapter_version = 1;
                // Simulate a retained old conversion, rather than merely testing
                // version equality: a stale frame must never reach the request.
                let frame = Arc::get_mut(&mut entry.frames[0]).unwrap();
                frame.items[0] = json!({"type":"message","id":"chatcmpl-stale"});
            }
            let lease = run.load(db, "t")?;
            assert_eq!(lease.materialize(), vec![json!({"type":"message","role":"assistant","content":[],"status":"completed"})]);
            let saved: String = db.query_row("SELECT payload_json FROM node WHERE id='n'", [], |r| r.get(0))?;
            assert_eq!(serde_json::from_str::<Value>(&saved).unwrap()["output"][0]["id"], "chatcmpl-old");
            assert_eq!(run.stats().parsed_nodes, 2);
            assert_eq!(run.stats().hits, 0);
            Ok(())
        }).await.unwrap();
        worker.shutdown().await.unwrap();
    }

    /// #186: each replayed item carries its source from the saved node kind;
    /// a spill's tool results are placeholders.
    #[tokio::test]
    async fn replayed_items_carry_their_saved_source_category() {
        use crate::model::wire_bytes::Category;
        let root = tempfile::tempdir().unwrap();
        let worker = crate::storage::DbWorker::open(root.path().join("db"))
            .await
            .unwrap();
        worker.call(|db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/AGENTS.md','p','m','now')", [])?;
            let tx = db.transaction()?;
            let text = |role: &str| json!({"role":role,"content":[{"type":"input_text","text":"x"}]});
            crate::history::append_node(&tx, "t", "u", None, "user", &text("user"), "now")?;
            crate::history::append_node(&tx, "t", "s", None, "summary", &text("developer"), "now")?;
            crate::history::append_node(&tx, "t", "c", None, "context", &text("developer"), "now")?;
            crate::history::append_node(&tx, "t", "e", None, "event", &text("developer"), "now")?;
            crate::history::append_node(&tx, "t", "p", None, "spill", &json!({"spill_version":1,"reasoning_epoch":0,"output":[
                {"type":"function_call","call_id":"k","name":"shell","arguments":"{}"},
                {"type":"function_call_output","call_id":"k","output":"{\"omitted\":true}"}]}), "now")?;
            tx.commit()?;
            let lease = load_uncached(db, "t", None, 0)?;
            let categories: Vec<_> = lease.sourced_items().map(|(_, category, _)| category).collect();
            assert_eq!(categories, vec![Category::User, Category::Summary, Category::Context, Category::Notice, Category::ToolCall, Category::SpillPlaceholder]);
            Ok(())
        }).await.unwrap();
        worker.shutdown().await.unwrap();
    }
}
