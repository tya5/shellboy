//! Bounded, width-independent source facts for positioned history reads.
use super::*;
use crate::history::payload::{LeafKind, visit};
use crate::protocol::{
    HistoryDisplayBlock as Block, HistoryDisplayField as Field, HistoryDisplayLink as Link,
    HistoryDisplayNode as Node, HistoryDisplayOutline as Outline, HistoryDisplayRange as Range,
    HistoryDisplayRole as Role, HistoryDisplayTool as Tool,
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
        .then(|| super::summary::decode(payload))
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
                .map_or_else(Vec::new, |value| budget.fields(value));
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
        budget.fields(payload)
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

    fn fields(&mut self, value: &Value) -> Vec<Field> {
        let candidates = fields_of_value(value);
        self.limited |= candidates.len() == MAX_FIELDS;
        candidates
            .into_iter()
            .filter_map(|field| {
                self.limited |= field.value_omitted;
                if self.fields == MAX_FIELDS {
                    self.limited = true;
                    return None;
                }
                if !self.admit(field.key.len() + field.value.len() + size_of::<Field>()) {
                    return None;
                }
                self.fields += 1;
                Some(Field {
                    path: field.key,
                    value: field.value,
                    omitted: field.value_omitted,
                })
            })
            .collect()
    }
}

fn detail_text(value: Option<&Value>, max: usize) -> Option<String> {
    value?
        .as_str()
        .map(|value| sanitize_plain(&short(value, max), 0).0)
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
        detail_text(get(key), size).filter(|value| budget.admit(value.len() + size_of::<String>()))
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
