//! Measured UTF-8 byte accounting of the final request body, by source (#186).
//!
//! Each canonical input item is tagged with its source category before dialect
//! conversion. Encoders carry the tag onto the wire fragments they build, and
//! the finished body is partitioned exactly: every byte of the compact JSON that
//! `send` serializes belongs to one category, and bytes no item produced (JSON
//! framing, model, tools, settings, cache hints) belong to `other`. These are
//! byte counts of what was sent, never token estimates.
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    io::{self, Write},
};

/// The source of request bytes. Names are the stable JSON keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    System,
    Instructions,
    Context,
    User,
    Assistant,
    Reasoning,
    ToolCall,
    ToolResult,
    SpillPlaceholder,
    Summary,
    Notice,
    Attachment,
    Other,
}
impl Category {
    pub const ALL: [Category; 13] = [
        Category::System,
        Category::Instructions,
        Category::Context,
        Category::User,
        Category::Assistant,
        Category::Reasoning,
        Category::ToolCall,
        Category::ToolResult,
        Category::SpillPlaceholder,
        Category::Summary,
        Category::Notice,
        Category::Attachment,
        Category::Other,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Category::System => "system",
            Category::Instructions => "instructions",
            Category::Context => "context",
            Category::User => "user",
            Category::Assistant => "assistant",
            Category::Reasoning => "reasoning",
            Category::ToolCall => "tool_call",
            Category::ToolResult => "tool_result",
            Category::SpillPlaceholder => "spill_placeholder",
            Category::Summary => "summary",
            Category::Notice => "notice",
            Category::Attachment => "attachment",
            Category::Other => "other",
        }
    }
    fn index(self) -> usize {
        Category::ALL
            .iter()
            .position(|c| *c == self)
            .expect("listed category")
    }
    /// The category of one replayed history item, from its saved node kind
    /// and its own shape. Spill nodes hold tool results as placeholders.
    pub fn of_history(kind: &str, spill: bool, item: &Value) -> Self {
        match kind {
            "context" => return Category::Context,
            "summary" | "compact_summary" => return Category::Summary,
            "event" => return Category::Notice,
            _ => {}
        }
        if crate::model::opaque::is_opaque(item) {
            return Category::Reasoning;
        }
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => Category::Reasoning,
            Some("function_call") => Category::ToolCall,
            Some("function_call_output") if spill => Category::SpillPlaceholder,
            Some("function_call_output") => Category::ToolResult,
            Some("message") | None => match item.get("role").and_then(Value::as_str) {
                Some("user") => Category::User,
                Some("assistant") => Category::Assistant,
                Some("system" | "developer") => Category::Notice,
                _ => Category::Other,
            },
            _ => Category::Other,
        }
    }
}

/// The source of one canonical input item, with per-content-part overrides
/// (expanded attachment content inside a user message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemTag {
    pub category: Category,
    pub parts: Vec<(usize, Category)>,
}
impl ItemTag {
    pub fn new(category: Category) -> Self {
        Self {
            category,
            parts: Vec::new(),
        }
    }
    /// The category of content part `index` of this item.
    pub fn part(&self, index: usize) -> Category {
        self.parts
            .iter()
            .find(|(at, _)| *at == index)
            .map_or(self.category, |(_, category)| *category)
    }
}

/// Measured bytes of one request body: the exact serialized length and its
/// partition. The categories always sum to `total`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputBytes {
    pub total: u64,
    counts: [u64; 13],
}
impl InputBytes {
    pub fn get(&self, category: Category) -> u64 {
        self.counts[category.index()]
    }
    /// `{"unit":"utf8_bytes","total":N,"categories":{name:N,...}}`, every
    /// category present (zero is a measurement).
    pub fn to_value(&self) -> Value {
        let mut categories = Map::new();
        for category in Category::ALL {
            categories.insert(category.name().into(), self.get(category).into());
        }
        serde_json::json!({"unit":"utf8_bytes","total":self.total,"categories":categories})
    }
    /// Validate a saved value: the fixed shape, nonnegative integers, and a sum
    /// equal to the total. Anything else is unknown (`None`), never repaired.
    pub fn from_value(value: &Value) -> Option<Self> {
        if value.get("unit")?.as_str()? != "utf8_bytes" {
            return None;
        }
        let total = value.get("total")?.as_u64()?;
        let saved = value.get("categories")?.as_object()?;
        if saved.len() != Category::ALL.len() {
            return None;
        }
        let mut counts = [0; 13];
        for category in Category::ALL {
            counts[category.index()] = saved.get(category.name())?.as_u64()?;
        }
        let sum = counts
            .iter()
            .try_fold(0u64, |sum, count| sum.checked_add(*count))?;
        (sum == total).then_some(Self { total, counts })
    }
}

/// How a labelled wire value is charged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Label {
    /// The whole value, unless a nested value has its own label.
    Whole(Category),
    /// A string assembled from pieces: each piece's escaped bytes go to its
    /// category; the quotes and separators stay with the enclosing value.
    Split(Vec<(Category, u64)>),
}
pub type Labels = Vec<(String, Label)>;

/// Local-only key carrying a tag on a wire fragment while it is built. It is
/// removed by [`extract`] before the body leaves the encoder.
const MARK: &str = "\u{0}shellboy_byte_category";

/// Tag a wire fragment (an object) with its source, when accounting.
pub(crate) fn mark(value: &mut Value, category: Option<Category>) {
    if let (Some(category), Some(object)) = (category, value.as_object_mut()) {
        object.insert(MARK.into(), (category.index() as u64).into());
    }
}
/// The tag of a fragment, if any.
pub(crate) fn marked(value: &Value) -> Option<Category> {
    let index = value.get(MARK)?.as_u64()?;
    Category::ALL.get(usize::try_from(index).ok()?).copied()
}

/// Remove every tag from `wire`, returning each tagged value's JSON pointer.
/// A cache hint inside a tagged value is request configuration: `other`.
pub(crate) fn extract(wire: &mut Value) -> Labels {
    let mut labels = Vec::new();
    let mut pointer = String::new();
    strip(wire, &mut pointer, &mut labels);
    labels
}
fn strip(value: &mut Value, pointer: &mut String, labels: &mut Labels) {
    let length = pointer.len();
    match value {
        Value::Object(object) => {
            if let Some(category) = object.remove(MARK).and_then(|index| {
                Category::ALL
                    .get(usize::try_from(index.as_u64()?).ok()?)
                    .copied()
            }) {
                labels.push((pointer.clone(), Label::Whole(category)));
            }
            for (key, child) in object.iter_mut() {
                pointer.push('/');
                pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
                if key == "cache_control" {
                    labels.push((pointer.clone(), Label::Whole(Category::Other)));
                }
                strip(child, pointer, labels);
                pointer.truncate(length);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                pointer.push('/');
                pointer.push_str(&index.to_string());
                strip(child, pointer, labels);
                pointer.truncate(length);
            }
        }
        _ => {}
    }
}

/// Partition the compact serialization of `wire` (exactly what `send` writes)
/// by label. Each byte belongs to the innermost labelled value containing it;
/// a member's key, colon and one separating comma go with the member's label.
/// Returns `None` when a label does not resolve or is duplicated.
pub fn measure(wire: &Value, labels: &[(String, Label)]) -> Option<InputBytes> {
    let mut resolved: HashMap<*const Value, &Label> = HashMap::with_capacity(labels.len());
    for (pointer, label) in labels {
        let target = wire.pointer(pointer)? as *const Value;
        if resolved.insert(target, label).is_some() {
            return None;
        }
    }
    let mut counts = [0u64; 13];
    walk(wire, Category::Other, &resolved, &mut counts)?;
    let total = serialized_len(wire);
    let sum = counts.iter().sum::<u64>();
    (sum == total).then_some(InputBytes { total, counts })
}

fn walk(
    value: &Value,
    enclosing: Category,
    labels: &HashMap<*const Value, &Label>,
    counts: &mut [u64; 13],
) -> Option<()> {
    let label = labels.get(&(value as *const Value)).copied();
    let own = match label {
        Some(Label::Whole(category)) => *category,
        _ => enclosing,
    };
    let member = |child: &Value| match labels.get(&(child as *const Value)) {
        Some(Label::Whole(category)) => Some(*category),
        _ => None,
    };
    // The comma before a member goes with that member when it is labelled.
    // A labelled first member has no comma before it, so it takes the one
    // after it instead; adding a labelled member (a cache hint) to an object
    // then never moves a comma between its other members' sources.
    let comma = |previous: Option<(usize, Option<Category>)>, current: Option<Category>| match (
        previous, current,
    ) {
        (None, _) => None,
        (_, Some(category)) => Some(category),
        (Some((0, Some(category))), None) => Some(category),
        _ => Some(own),
    };
    match value {
        Value::Object(object) => {
            counts[own.index()] += 2;
            let mut previous = None;
            for (index, (key, child)) in object.iter().enumerate() {
                let current = member(child);
                if let Some(category) = comma(previous, current) {
                    counts[category.index()] += 1;
                }
                counts[current.unwrap_or(own).index()] += string_len(key) + 1;
                walk(child, own, labels, counts)?;
                previous = Some((index, current));
            }
        }
        Value::Array(items) => {
            counts[own.index()] += 2;
            let mut previous = None;
            for (index, child) in items.iter().enumerate() {
                let current = member(child);
                if let Some(category) = comma(previous, current) {
                    counts[category.index()] += 1;
                }
                walk(child, own, labels, counts)?;
                previous = Some((index, current));
            }
        }
        _ => {
            let length = serialized_len(value);
            if let Some(Label::Split(pieces)) = label {
                let assigned = pieces.iter().map(|(_, bytes)| *bytes).sum::<u64>();
                for (category, bytes) in pieces {
                    counts[category.index()] += bytes;
                }
                counts[own.index()] += length.checked_sub(assigned)?;
            } else {
                counts[own.index()] += length;
            }
        }
    }
    Some(())
}

/// Escaped bytes of `text` inside a JSON string, without the quotes. JSON
/// escaping is per character, so the pieces of a joined string add up.
pub(crate) fn escaped_len(text: &str) -> u64 {
    string_len(text) - 2
}
fn string_len(text: &str) -> u64 {
    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, text).expect("a string serializes as JSON");
    counter.0
}

#[derive(Default)]
struct Counter(u64);
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_len(value: &Value) -> u64 {
    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value).expect("JSON Value is serializable");
    counter.0
}

#[cfg(test)]
mod tests;
