//! Immutable Responses items with one byte-bounded LRU shared by all Threads.
use crate::storage::StorageError;
use rusqlite::Connection;
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    mem::size_of,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const ADAPTER_VERSION: u32 = 1;
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
}
#[derive(Default)]
struct Ledger {
    live: AtomicUsize,
    pinned: AtomicUsize,
}
struct Frame {
    stamp: Stamp,
    items: Vec<Value>,
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
    fn new(frames: Vec<Arc<Frame>>) -> Self {
        for frame in &frames {
            if frame.pins.fetch_add(1, Ordering::AcqRel) == 0 {
                frame.ledger.pinned.fetch_add(frame.bytes, Ordering::AcqRel);
            }
        }
        Self(Arc::new(LeaseFrames { frames }))
    }
    pub fn items(&self) -> impl Iterator<Item = &Value> {
        self.0.frames.iter().flat_map(|frame| &frame.items)
    }
    /// Keep provenance with cached conversion; variable attachment bytes are
    /// bound after this immutable text plan has been loaded.
    pub(crate) fn sourced_items(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.frames.iter().flat_map(|frame| {
            frame
                .items
                .iter()
                .map(|item| (frame.stamp.id.as_str(), item))
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
        let stamps=db.prepare("SELECT n.id,n.payload_revision,n.deleted_reason IS NOT NULL OR n.payload_json IS NULL FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 ORDER BY p.ordinal")?.query_map([thread],|row|Ok(Stamp{id:row.get(0)?,revision:row.get(1)?,tombstone:row.get(2)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
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
                let items = if stamp.tombstone {
                    Vec::new()
                } else {
                    let body: String = row.get(1)?;
                    let value: Value = serde_json::from_str(&body)
                        .map_err(|error| StorageError::Schema(error.to_string()))?;
                    state.parsed_nodes += 1;
                    convert(value, context.epoch)
                };
                let bytes = size_of::<Frame>()
                    + 2 * size_of::<usize>()
                    + stamp.id.capacity()
                    + items.capacity() * size_of::<Value>()
                    + items.iter().map(heap_bytes).sum::<usize>();
                self.ledger.live.fetch_add(bytes, Ordering::AcqRel);
                frames.push(Arc::new(Frame {
                    stamp,
                    items,
                    bytes,
                    pins: AtomicUsize::new(0),
                    ledger: self.ledger.clone(),
                }));
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
        let lease = ReplayLease::new(frames.clone());
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
fn convert(mut value: Value, epoch: i64) -> Vec<Value> {
    let stored_epoch = value.get("reasoning_epoch").and_then(Value::as_i64);
    if let Some(output) = value.get_mut("output").and_then(Value::as_array_mut) {
        return std::mem::take(output)
            .into_iter()
            .filter(|item| {
                item.get("type").and_then(Value::as_str) != Some("reasoning")
                    || stored_epoch == Some(epoch)
            })
            .collect();
    }
    if let Some(object) = value.as_object_mut() {
        if let Some(item) = object.remove("item") {
            return vec![item];
        }
        if object.contains_key("role") {
            object.remove("event_id");
            return vec![value];
        }
    }
    Vec::new()
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
