//! Owner-side display outline: bounded, width-independent source facts for positioned history
//! reads, with the field, envelope and summary decoding it needs. Presentation imports these rules.
use crate::plain_text::{MAX_LINKS, MAX_METADATA_BYTES, sanitize_plain};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::mem::size_of;

pub(crate) mod summary;

pub(crate) const MAX_FIELDS: usize = 128;
pub(crate) const MAX_BLOCKS: usize = 128;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationField {
    #[serde(default)]
    pub value_omitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    pub key: String,
    pub label: String,
    pub value: String,
}
impl PresentationField {
    pub fn displayed(&self) -> &str {
        self.display.as_deref().unwrap_or(&self.value)
    }
}
pub(crate) fn short(text: &str, limit: usize) -> Text {
    let prefix: String = text.chars().take(limit).collect();
    Text {
        truncated: prefix.len() < text.len(),
        text: prefix,
    }
}

pub(crate) struct FieldFact {
    pub path: String,
    pub value: Text,
    pub omitted: Option<Omission>,
    pub count: bool,
}

pub(crate) fn secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "encrypted_content",
        "encryptedcontent",
        "origin_token",
        "authorization",
        "api_key",
        "api-key",
        "apikey",
        "secret",
        "password",
        "access_token",
        "refresh_token",
    ]
    .iter()
    .any(|s| key.contains(s))
        || key == "token"
}
// Persisted envelope values may be scalar JSON. Detail decoding intentionally
// accepts only containers and trims before checking their shape.
pub(crate) fn parse_envelope(text: &str) -> Option<Value> {
    serde_json::from_str(text.strip_prefix("Shellboy event v1: ").unwrap_or(text)).ok()
}
pub(crate) fn parse_container_detail(text: &str) -> Option<Value> {
    let body = text
        .strip_prefix("Shellboy event v1: ")
        .unwrap_or(text)
        .trim();
    if body.starts_with('{') || body.starts_with('[') {
        serde_json::from_str(body).ok()
    } else {
        None
    }
}

/// The saved payload's shape along one field pointer, as `human_label_for`
/// navigates it: object keys, array positions, and `Count` after a collection.
pub(crate) fn path_shape(
    pointer: &str,
    root: &Value,
) -> Vec<crate::protocol::HistoryDisplayPathSegment> {
    use crate::protocol::HistoryDisplayPathSegment as Segment;
    let mut shape = Vec::new();
    let mut value = root;
    for key in pointer
        .trim_start_matches('/')
        .split('/')
        .map(crate::json_pointer::unescape_segment)
    {
        if let Value::Array(items) = value
            && let Ok(position) = key.parse::<usize>()
        {
            value = items.get(position).unwrap_or(&Value::Null);
            shape.push(Segment::Index {
                index: position as u64,
            });
        } else {
            value = value.get(key.as_str()).unwrap_or(&Value::Null);
            shape.push(Segment::Key { key });
        }
        if value.is_array() {
            shape.push(Segment::Count);
        }
    }
    shape
}

pub(crate) fn fields_of_value(value: &Value) -> Vec<FieldFact> {
    fn walk(value: &Value, path: &str, out: &mut Vec<FieldFact>, used: &mut usize, depth: usize) {
        if out.len() >= MAX_FIELDS || *used >= MAX_METADATA_BYTES {
            return;
        }
        if depth > 8 {
            out.push(FieldFact {
                path: path.into(),
                value: short("", 0),
                omitted: Some(Omission::DeepStructure),
                count: false,
            });
            return;
        }
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if secret(key) {
                        continue;
                    }
                    let path = if path.is_empty() {
                        format!("/{}", crate::json_pointer::escape_segment(key))
                    } else {
                        format!("{path}/{}", crate::json_pointer::escape_segment(key))
                    };
                    walk(value, &path, out, used, depth + 1);
                }
            }
            Value::Array(items) => {
                out.push(FieldFact {
                    path: path.into(),
                    value: Text {
                        text: items.len().to_string(),
                        truncated: false,
                    },
                    omitted: None,
                    count: true,
                });
                for (i, value) in items.iter().take(MAX_FIELDS).enumerate() {
                    walk(value, &format!("{path}/{i}"), out, used, depth + 1);
                }
            }
            _ => {
                if let Value::String(s) = value
                    && let Some(parsed) = parse_container_detail(s)
                {
                    walk(&parsed, path, out, used, depth + 1);
                    return;
                }
                let value = match value {
                    Value::String(s) => bounded_plain(s, 1024),
                    _ => Text {
                        text: value.to_string(),
                        truncated: false,
                    },
                };
                *used += path.len()
                    + short(path, 256).text.len()
                    + value.text.len()
                    + size_of::<FieldFact>();
                if *used <= MAX_METADATA_BYTES {
                    out.push(FieldFact {
                        path: path.into(),
                        value,
                        omitted: None,
                        count: false,
                    });
                }
            }
        }
    }
    let mut fields = vec![];
    walk(value, "", &mut fields, &mut 0, 0);
    fields
}

use crate::history::payload::{LeafKind, visit};
use crate::protocol::{
    HistoryDisplayBlock as Block, HistoryDisplayField as Field, HistoryDisplayLink as Link,
    HistoryDisplayNode as Node, HistoryDisplayOmission as Omission,
    HistoryDisplayOutline as Outline, HistoryDisplayRange as Range, HistoryDisplayRole as Role,
    HistoryDisplayText as Text, HistoryDisplayTool as Tool,
};

pub(crate) fn display_outline(
    id: &str,
    kind: &str,
    payload: &Value,
    source: &str,
    range: &Range,
) -> Outline {
    let mut budget = Budget::default();
    let summary = matches!(kind, "summary" | "compact_summary")
        .then(|| summary::decode(payload))
        .flatten();
    let node = if crate::history::is_legacy_admission_node(id, kind) {
        Node::HiddenAdmission
    } else if let Some(reason) = detail_text(payload.get("tombstone_reason"), 256) {
        Node::Reclaimed { reason }
    } else if matches!(kind, "incomplete_response" | "model_stream_observation") {
        Node::Received
    } else if matches!(kind, "spill" | "context_trim")
        && let Some(trim) = payload.get("context_trim").filter(|trim| trim.is_object())
    {
        Node::Trim {
            omitted_nodes: trim["omitted_nodes"].as_u64(),
            reason: detail_text(trim.get("reason"), 256),
            first_node_id: exact(trim.get("first_node_id"), &mut budget),
            last_node_id: exact(trim.get("last_node_id"), &mut budget),
        }
    } else if kind == "spill" {
        Node::Spill {
            source_node_ids: source_ids(payload, &mut budget),
        }
    } else if matches!(kind, "summary" | "compact_summary") {
        Node::Summary {
            source_node_ids: source_ids(payload, &mut budget),
            structured: summary.is_some(),
        }
    } else if matches!(
        kind,
        "interruption_context"
            | "interruption_details"
            | "interrupted_run"
            | "interrupted_wait"
            | "interrupted_message"
    ) {
        Node::Interruption
    } else {
        Node::Ordinary
    };
    let mut blocks = Vec::new();
    let default = match kind {
        "assistant" | "assistant_response" | "summary" | "compact_summary" => Role::Prose,
        "reasoning" => Role::Reasoning,
        "tool_result" => Role::ToolResult,
        "event" => Role::Event,
        _ if matches!(node, Node::Interruption) => Role::Event,
        _ => Role::Plain,
    };
    if !matches!(node, Node::HiddenAdmission | Node::Received) {
        let mut push = |start: usize,
                        end: usize,
                        role: Role,
                        text: &str,
                        call: Option<&Value>,
                        ordinal: Option<u32>| {
            let start = range.start.max(start as u64);
            let end = range.end.min(end as u64);
            if start >= end {
                return;
            }
            if blocks.len() == MAX_BLOCKS {
                budget.limited = true;
                return;
            }
            let envelope = matches!(role, Role::ToolCall | Role::ToolResult | Role::Event)
                .then(|| parse_envelope(text))
                .flatten();
            let tool = matches!(role, Role::ToolCall | Role::ToolResult)
                .then(|| tool(call, envelope.as_ref(), payload, ordinal, &mut budget));
            let event_kind = (role == Role::Event)
                .then(|| {
                    let value = envelope.as_ref()?;
                    let event = if value["kind"].as_str() == Some("context_changed") {
                        value.get("payload").unwrap_or(value)
                    } else {
                        value
                    };
                    detail_text(event.get("kind"), 256)
                })
                .flatten();
            let fields = envelope
                .as_ref()
                .map_or_else(Vec::new, |value| budget.fields(value, payload));
            blocks.push(Block {
                range: Range { start, end },
                role,
                tool,
                event_kind,
                fields,
            });
        };
        visit(payload, |leaf| {
            let role = match leaf.context.kind {
                LeafKind::Natural => default,
                LeafKind::Reasoning => Role::Reasoning,
                LeafKind::ToolCall => Role::ToolCall,
                LeafKind::ToolResult => Role::ToolResult,
            };
            if let Some(summary) = summary.as_ref().filter(|summary| summary.body == leaf.text) {
                for part in &summary.parts {
                    push(
                        leaf.start + part.start,
                        leaf.start + part.end,
                        role,
                        part.text,
                        None,
                        None,
                    );
                }
            } else {
                push(
                    leaf.start,
                    leaf.end,
                    role,
                    leaf.text,
                    leaf.context.call,
                    leaf.context.call_ordinal,
                );
            }
        });
    }
    let fields = if matches!(node, Node::HiddenAdmission | Node::Received) {
        Vec::new()
    } else {
        budget.fields(payload, payload)
    };
    let mut links = Vec::new();
    if !matches!(node, Node::HiddenAdmission) {
        let (_, candidates) = sanitize_plain(source, range.start as usize);
        budget.limited |= candidates.len() == MAX_LINKS;
        for link in candidates {
            if blocks.iter().any(|block| {
                matches!(block.role, Role::ToolCall | Role::ToolResult | Role::Event)
                    && block.range.start < link.source_end as u64
                    && (link.source_start as u64) < block.range.end
            }) {
                continue;
            }
            if budget.admit(link.url.len() + link.label.len() + size_of::<Link>()) {
                links.push(Link {
                    range: Range {
                        start: link.source_start as u64,
                        end: link.source_end as u64,
                    },
                    url: link.url,
                    label: link.label,
                });
            }
        }
    }
    Outline {
        node,
        blocks,
        fields,
        links,
        limited: budget.limited,
    }
}

#[derive(Default)]
struct Budget {
    bytes: usize,
    fields: usize,
    limited: bool,
}
impl Budget {
    fn admit(&mut self, bytes: usize) -> bool {
        if bytes > MAX_METADATA_BYTES.saturating_sub(self.bytes) {
            self.limited = true;
            return false;
        }
        self.bytes += bytes;
        true
    }

    /// Labels follow the saved payload's shape (`root`), as in a payload projection.
    fn fields(&mut self, value: &Value, root: &Value) -> Vec<Field> {
        let candidates = fields_of_value(value);
        self.limited |= candidates.len() == MAX_FIELDS;
        candidates
            .into_iter()
            .filter_map(|field| {
                self.limited |= field.value.truncated || field.omitted.is_some();
                if self.fields == MAX_FIELDS {
                    self.limited = true;
                    return None;
                }
                if !self.admit(field.path.len() + field.value.text.len() + size_of::<Field>()) {
                    return None;
                }
                self.fields += 1;
                Some(Field {
                    shape: path_shape(&field.path, root),
                    path: field.path,
                    value: field.value.text,
                    truncated: field.value.truncated,
                    omitted: field.omitted,
                })
            })
            .collect()
    }
}

fn bounded_plain(value: &str, max: usize) -> Text {
    let mut bounded = short(value, max);
    bounded.text = sanitize_plain(&bounded.text, 0).0;
    bounded
}

fn detail_text(value: Option<&Value>, max: usize) -> Option<Text> {
    value?.as_str().map(|value| bounded_plain(value, max))
}

fn exact(value: Option<&Value>, budget: &mut Budget) -> Option<String> {
    let value = value?.as_str()?;
    if value.is_empty() {
        return None;
    }
    if value.len() > 256 || value.chars().any(char::is_control) {
        budget.limited = true;
        return None;
    }
    budget
        .admit(value.len() + size_of::<String>())
        .then(|| value.to_owned())
}

fn source_ids(payload: &Value, budget: &mut Budget) -> Vec<String> {
    let Some(ids) = payload.get("source_node_ids").and_then(Value::as_array) else {
        return vec![];
    };
    budget.limited |= ids.len() > MAX_FIELDS;
    ids.iter()
        .take(MAX_FIELDS)
        .filter_map(|id| exact(Some(id), budget))
        .collect()
}

fn tool(
    call: Option<&Value>,
    envelope: Option<&Value>,
    root: &Value,
    ordinal: Option<u32>,
    budget: &mut Budget,
) -> Tool {
    let get = |key| envelope.and_then(|value| value.get(key));
    let mut text = |key, size| {
        detail_text(get(key), size)
            .filter(|value| budget.admit(value.text.len() + size_of::<Text>()))
    };
    let command = text("command", 512);
    let description = text("description", 256);
    let status = text("status", 128);
    Tool {
        call_id: exact(
            call.and_then(|call| call.get("call_id"))
                .or_else(|| get("call_id")),
            budget,
        ),
        ordinal,
        group_id: exact(root.get("group_id").or_else(|| get("group_id")), budget),
        execution_id: exact(get("execution_id"), budget),
        command,
        description,
        status,
        exit_code: get("exit_code").and_then(Value::as_i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_outline_renders_typed_facts_without_rewriting_literal_ellipsis() {
        use crate::config::UiLanguage;
        use crate::ui::text_width::{clear_terminal_width, set_terminal_width};
        let mut deep = serde_json::json!("hidden");
        for _ in 0..10 {
            deep = serde_json::json!({"k": deep});
        }
        let payload = serde_json::json!({"deep": deep, "long": "x".repeat(1025), "literal": "x…"});
        let outline = display_outline("fixture", "user", &payload, "", &Range { start: 0, end: 0 });
        set_terminal_width(Some(2));
        let en = crate::presentation::project_display("user", "", 0, &outline, UiLanguage::En);
        let ja = crate::presentation::project_display("user", "", 0, &outline, UiLanguage::Ja);
        clear_terminal_width();
        assert!(
            en.fields
                .iter()
                .any(|f| f.key == "/long" && f.value.ends_with("..."))
        );
        assert!(
            en.fields
                .iter()
                .any(|f| f.key == "/literal" && f.value == "x…")
        );
        assert!(
            en.fields
                .iter()
                .any(|f| f.value == "[Deep structure | see original]")
        );
        assert!(
            ja.fields
                .iter()
                .any(|f| f.value == "[深い構造 | 詳細は原文参照]")
        );
    }

    #[test]
    fn owner_outline_reports_truncation_and_deep_omission_without_rendered_text() {
        let mut deep = serde_json::json!("hidden");
        for _ in 0..10 {
            deep = serde_json::json!({ "k": deep });
        }
        let payload = serde_json::json!({"deep": deep, "long": "x".repeat(1025), "literal": "x…"});
        let range = Range { start: 0, end: 0 };
        let outline = display_outline("fixture", "user", &payload, "", &range);
        let long = outline.fields.iter().find(|f| f.path == "/long").unwrap();
        assert_eq!(long.value, "x".repeat(1024));
        assert!(long.truncated);
        let deep = outline.fields.iter().find(|f| f.omitted.is_some()).unwrap();
        assert_eq!(deep.omitted, Some(Omission::DeepStructure));
        assert!(deep.value.is_empty());
        let literal = outline
            .fields
            .iter()
            .find(|f| f.path == "/literal")
            .unwrap();
        assert_eq!(literal.value, "x…");
        assert!(!literal.truncated);
    }
}
