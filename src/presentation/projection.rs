//! Immutable display projection phases over the shared history payload visitor.
use super::*;
mod envelopes;
mod limits;

/// Project only the supplied canonical text window; payload is inspected without mutation.
pub fn project(
    kind: &str,
    payload: &Value,
    text_window: &str,
    base_char_offset: usize,
) -> NodePresentation {
    let (mut p, block_kind) = initial(kind, payload);
    let decoded_summary = summary_projection(&mut p, payload);
    let walk = project_window(
        payload,
        text_window,
        base_char_offset,
        &block_kind,
        decoded_summary.as_ref(),
    );
    p.body_omitted_chars = walk.omitted_chars;
    p.blocks = walk.blocks;
    p.links = walk.links;
    // Bound retained tokens before allocating decoded envelope fields/output.
    limits::markdown(&mut p);
    fallback_window(
        &mut p,
        text_window,
        base_char_offset,
        block_kind,
        decoded_summary.is_some(),
    );
    envelopes::apply(&mut p, kind, base_char_offset, walk.envelopes.into_iter());
    limits::finish(&mut p, payload);
    p
}

fn initial(kind: &str, payload: &Value) -> (NodePresentation, BlockKind) {
    let (display_kind, title, block_kind) = match kind {
        "user" | "user_message" | "message" => (PresentationKind::User, "あなた", BlockKind::Plain),
        "assistant_response" | "assistant" => {
            (PresentationKind::Assistant, "Shellboy", BlockKind::Prose)
        }
        "reasoning" => (PresentationKind::Assistant, "推論", BlockKind::Reasoning),
        "tool_result" => (
            PresentationKind::ToolResult,
            "shell結果",
            BlockKind::ToolResult,
        ),
        "event" => (PresentationKind::Event, "状態変更", BlockKind::Event),
        "summary" | "compact_summary" => (PresentationKind::Summary, "要約", BlockKind::Prose),
        "spill" => (PresentationKind::Spill, "原文参照", BlockKind::Plain),
        "interrupted_run"
        | "interrupted_wait"
        | "interrupted_message"
        | "interruption_context"
        | "interruption_details" => (
            PresentationKind::Interrupted,
            "前回処理の中断情報",
            BlockKind::Event,
        ),
        _ => (PresentationKind::Unknown, "未対応の項目", BlockKind::Plain),
    };
    let mut p = NodePresentation {
        kind: display_kind,
        title: title.into(),
        ..Default::default()
    };
    p.fields = fields_of_value(payload);
    if let Some(reason) = payload.get("tombstone_reason").and_then(Value::as_str) {
        p.title = format!("原文回収済み · {reason}");
        p.kind = PresentationKind::Unknown;
    }
    if p.kind == PresentationKind::Unknown && payload.get("tombstone_reason").is_none() {
        p.title = format!("未対応の項目: {kind}");
    }
    (p, block_kind)
}

struct WindowProjection<'a> {
    window: &'a str,
    base: usize,
    end: usize,
    blocks: Vec<PresentationBlock>,
    links: Vec<PresentationLink>,
    envelopes: Vec<Option<Value>>,
    omitted_chars: usize,
}
impl WindowProjection<'_> {
    fn leaf(
        &mut self,
        text: &str,
        kind: BlockKind,
        start: usize,
        end: usize,
        call: Option<&Value>,
        ordinal: Option<u32>,
    ) {
        let original_text = text;
        let lo = start.max(self.base);
        let hi = end.min(self.end);
        if lo >= hi {
            return;
        }
        if self.blocks.len() >= MAX_BLOCKS {
            self.omitted_chars = self.omitted_chars.saturating_add(hi - lo);
            return;
        }
        // Read only the canonical window, never a second whole-payload text clone.
        let source: String = self
            .window
            .chars()
            .skip(lo - self.base)
            .take(hi - lo)
            .collect();
        let (text, links) = sanitize_plain(&source, lo);
        for l in links {
            add_link(
                &mut self.links,
                &l.label,
                &l.url,
                l.source_start,
                l.source_end,
            );
        }
        let markdown = if matches!(kind, BlockKind::Prose | BlockKind::Reasoning) {
            markdown(&source, lo, &mut self.links)
        } else {
            vec![]
        };
        self.envelopes.push(
            if matches!(
                kind,
                BlockKind::ToolCall | BlockKind::ToolResult | BlockKind::Event
            ) {
                parse_envelope(original_text)
            } else {
                None
            },
        );
        let source_exact = text == source;
        self.blocks.push(PresentationBlock {
            kind: kind.clone(),
            stream: None,
            tool: call.map(|call| ToolPresentation {
                call_id: call
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_default(),
                ordinal,
                status: if kind == BlockKind::ToolCall {
                    "requested".into()
                } else {
                    String::new()
                },
                ..Default::default()
            }),
            text,
            source_start: lo,
            source_end: hi,
            source_exact,
            markdown,
            rendered: None,
        });
    }
}

fn summary_projection<'a>(
    p: &mut NodePresentation,
    payload: &'a Value,
) -> Option<summary::Decoded<'a>> {
    let decoded_summary = if p.kind == PresentationKind::Summary {
        summary::decode(payload)
    } else {
        None
    };
    if p.kind == PresentationKind::Summary
        && let Some(sources) = payload
            .get("source_node_ids")
            .and_then(Value::as_array)
            .filter(|sources| sources.iter().all(Value::is_string))
    {
        p.title = if decoded_summary.is_some() {
            format!(
                "要約 · 置換{}件 · 出典{}件 · F6→O 原文",
                sources.len(),
                sources.len()
            )
        } else {
            format!("要約 · 出典{}件 · F6→O 原文", sources.len())
        };
    }
    decoded_summary
}

fn project_window<'a>(
    payload: &Value,
    text_window: &'a str,
    base_char_offset: usize,
    block_kind: &BlockKind,
    decoded_summary: Option<&summary::Decoded<'_>>,
) -> WindowProjection<'a> {
    let mut walk = WindowProjection {
        window: text_window,
        base: base_char_offset,
        end: base_char_offset + text_window.chars().count(),
        blocks: vec![],
        links: vec![],
        envelopes: vec![],
        omitted_chars: 0,
    };
    crate::history::payload::visit(payload, |leaf| {
        let kind = match leaf.context.kind {
            crate::history::payload::LeafKind::Natural => block_kind.clone(),
            crate::history::payload::LeafKind::Reasoning => BlockKind::Reasoning,
            crate::history::payload::LeafKind::ToolCall => BlockKind::ToolCall,
            crate::history::payload::LeafKind::ToolResult => BlockKind::ToolResult,
        };
        if let Some(summary) = decoded_summary.filter(|summary| summary.body == leaf.text) {
            for part in &summary.parts {
                walk.leaf(
                    part.text,
                    kind.clone(),
                    leaf.start + part.start,
                    leaf.start + part.end,
                    None,
                    None,
                );
            }
        } else {
            walk.leaf(
                leaf.text,
                kind,
                leaf.start,
                leaf.end,
                leaf.context.call,
                leaf.context.call_ordinal,
            );
        }
    });
    walk
}

fn fallback_window(
    p: &mut NodePresentation,
    text_window: &str,
    base_char_offset: usize,
    block_kind: BlockKind,
    has_summary: bool,
) {
    if p.blocks.is_empty() && has_summary && !text_window.is_empty() {
        // A source window containing only generated headers/footers still owns
        // its canonical anchor, but must not fall back to rendering UUID lines.
        p.blocks.push(PresentationBlock {
            kind: BlockKind::Plain,
            source_start: base_char_offset,
            source_end: base_char_offset + text_window.chars().count(),
            ..Default::default()
        });
    }
    if p.blocks.is_empty() && !text_window.is_empty() {
        let (text, links) = sanitize_plain(text_window, base_char_offset);
        p.links = links;
        p.blocks.push(PresentationBlock {
            kind: block_kind,
            stream: None,
            tool: None,
            text,
            source_start: base_char_offset,
            source_end: base_char_offset + text_window.chars().count(),
            source_exact: true,
            markdown: vec![],
            rendered: None,
        });
    }
}
