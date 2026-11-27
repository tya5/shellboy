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

/// Project a verified native extent without reconstructing a storage payload.
pub(crate) fn project_display(
    kind: &str,
    text: &str,
    base: usize,
    outline: &crate::protocol::HistoryDisplayOutline,
) -> NodePresentation {
    use crate::protocol::{HistoryDisplayNode as Node, HistoryDisplayRole as Role};
    let (mut p, fallback) = initial(kind, &Value::Null);
    let mut structured = false;
    match &outline.node {
        Node::HiddenAdmission | Node::Received => return p,
        Node::Trim { .. } => set_trim_title(&mut p),
        Node::Reclaimed { reason } => set_reclaimed_title(&mut p, &outline_text(reason)),
        Node::Summary {
            source_node_ids,
            structured: parts,
        } => {
            structured = *parts;
            set_summary_title(&mut p, source_node_ids.len(), structured);
        }
        _ => {}
    }
    // Provisional labels match the saved-field walk until the shapes rename them.
    let field = |f: &crate::protocol::HistoryDisplayField| PresentationField {
        key: f.path.clone(),
        label: if matches!(
            f.shape.last(),
            Some(crate::protocol::HistoryDisplayPathSegment::Count)
        ) {
            format!("{} 件数", f.path)
        } else {
            short(&f.path, 256)
        },
        value: render_outline_text(f.value.clone(), f.truncated),
        value_omitted: f.truncated || f.omitted.is_some(),
        omission: f.omitted,
        ..Default::default()
    };
    p.fields = outline.fields.iter().map(field).collect();
    let mut walk = WindowProjection {
        window: text,
        base,
        end: base + text.chars().count(),
        blocks: Vec::new(),
        links: Vec::new(),
        envelopes: Vec::new(),
        omitted_chars: 0,
    };
    let offsets = text
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    for block in &outline.blocks {
        let start = block.range.start as usize;
        let end = block.range.end as usize;
        let Some((&lo, &hi)) = offsets
            .get(start.saturating_sub(base))
            .zip(offsets.get(end.saturating_sub(base)))
        else {
            continue;
        };
        let role = match block.role {
            Role::Plain => BlockKind::Plain,
            Role::Prose => BlockKind::Prose,
            Role::Reasoning => BlockKind::Reasoning,
            Role::ToolCall => BlockKind::ToolCall,
            Role::ToolResult => BlockKind::ToolResult,
            Role::Event => BlockKind::Event,
        };
        let is_call = role == BlockKind::ToolCall;
        walk.leaf(&text[lo..hi], role, start, end, None, None);
        if let Some(saved) = &block.tool
            && let Some(presented) = walk.blocks.last_mut()
        {
            presented.tool = Some(ToolPresentation {
                call_id: saved.call_id.clone().unwrap_or_default(),
                group_id: saved.group_id.clone(),
                ordinal: saved.ordinal,
                execution_id: saved.execution_id.clone(),
                command: saved.command.as_ref().map(outline_text).unwrap_or_default(),
                description: crate::tool_description::label(
                    saved.description.as_ref().map(outline_text).as_deref(),
                ),
                status: saved.status.as_ref().map(outline_text).unwrap_or_else(|| {
                    if is_call {
                        "requested".into()
                    } else {
                        String::new()
                    }
                }),
                exit_code: saved.exit_code,
                ..Default::default()
            });
        }
        if walk.envelopes.last().is_some_and(Option::is_none) {
            p.fields.extend(block.fields.iter().map(field));
        }
    }
    p.blocks = walk.blocks;
    // Links come from the same extent text and walk as a payload projection;
    // the outline's own links serve clients without this Markdown walk.
    p.links = walk.links;
    p.metadata_limited = outline.limited;
    limits::markdown(&mut p);
    if !outline.limited || structured {
        fallback_window(&mut p, text, base, fallback, structured);
    }
    envelopes::apply(&mut p, kind, base, walk.envelopes.into_iter());
    // Labels that depend on the saved payload come from the owner's path shapes.
    let shapes = outline
        .fields
        .iter()
        .chain(outline.blocks.iter().flat_map(|block| &block.fields))
        .map(|field| (field.path.as_str(), field.shape.as_slice()))
        .collect::<std::collections::HashMap<_, _>>();
    limits::finish_shaped(&mut p, &shapes);
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
        kind: display_kind.clone(),
        title: title.into(),
        ..Default::default()
    };
    use super::messages::{Message, Text};
    p.ui_messages.title = Some(match display_kind {
        PresentationKind::User => Message::Text(Text::User),
        PresentationKind::Assistant if kind == "reasoning" => Message::Text(Text::Reasoning),
        PresentationKind::Assistant => Message::Raw {
            text: "Shellboy".into(),
        },
        PresentationKind::ToolResult => Message::Text(Text::ToolResult),
        PresentationKind::Event => Message::Text(Text::Event),
        PresentationKind::Summary => Message::Text(Text::Summary),
        PresentationKind::Spill => Message::Text(Text::Spill),
        PresentationKind::Interrupted => Message::Text(Text::Interrupted),
        _ => Message::UnknownTitle {
            kind: short(kind, 64),
        },
    });
    p.fields = fields_of_value(payload);
    if matches!(kind, "spill" | "context_trim")
        && payload.get("context_trim").is_some_and(Value::is_object)
    {
        // A trim is a reversible input replacement, not a routine lifecycle
        // event. Keep its notice visible and its canonical text unchanged.
        set_trim_title(&mut p);
    }
    if let Some(reason) = payload.get("tombstone_reason").and_then(Value::as_str) {
        set_reclaimed_title(&mut p, reason);
    }
    if p.kind == PresentationKind::Unknown && payload.get("tombstone_reason").is_none() {
        p.title = format!("未対応の項目: {kind}");
    }
    (p, block_kind)
}

fn set_trim_title(p: &mut NodePresentation) {
    p.kind = PresentationKind::Spill;
    p.title = CONTEXT_TRIM_TITLE.into();
    p.ui_messages.title = Some(messages::Message::Text(messages::Text::ContextTrim));
}

fn set_reclaimed_title(p: &mut NodePresentation, reason: &str) {
    p.ui_messages.title = Some(messages::Message::Reclaimed {
        reason: short(reason, 256),
    });
    p.title = crate::ui::text_width::glyphs::formatted!(
        "原文回収済み · {reason}",
        "原文回収済み | {reason}"
    );
    p.kind = PresentationKind::Unknown;
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
        set_summary_title(p, sources.len(), decoded_summary.is_some());
    }
    decoded_summary
}

fn set_summary_title(p: &mut NodePresentation, count: usize, replaced: bool) {
    p.ui_messages.title = Some(messages::Message::Summary { count, replaced });
    p.title = if replaced {
        crate::ui::text_width::glyphs::formatted!(
            "要約 · 置換{}件 · 出典{}件 · F6→O 原文",
            "要約 | 置換{}件 | 出典{}件 | F6>O 原文",
            count,
            count
        )
    } else {
        crate::ui::text_width::glyphs::formatted!(
            "要約 · 出典{}件 · F6→O 原文",
            "要約 | 出典{}件 | F6>O 原文",
            count
        )
    };
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

#[cfg(test)]
mod language_audit_tests {
    use super::*;
    use crate::config::UiLanguage::{En, Ja};

    #[test]
    fn generated_titles_pair_japanese_with_english_without_changing_source_values() {
        for (kind, japanese, english) in [
            ("user", "あなた", "You"),
            ("reasoning", "推論", "Reasoning"),
            ("summary", "要約", "Summary"),
            ("spill", "原文参照", "Original reference"),
            (
                "future-kind",
                "未対応の項目: future-kind",
                "Unsupported item: future-kind",
            ),
        ] {
            let (projected, _) = initial(kind, &serde_json::json!({}));
            let title = projected.ui_messages.title.as_ref().unwrap();
            assert_eq!(projected.title, japanese);
            assert_eq!(title.render(Ja), japanese);
            assert_eq!(title.render(En), english);
        }
        let (reclaimed, _) = initial(
            "future-kind",
            &serde_json::json!({"tombstone_reason":"saved raw"}),
        );
        let title = reclaimed.ui_messages.title.as_ref().unwrap();
        assert_eq!(reclaimed.title, "原文回収済み | saved raw");
        assert_eq!(title.render(Ja), reclaimed.title);
        assert_eq!(title.render(En), "Original reclaimed | saved raw");
    }
    #[test]
    fn remaining_generated_titles_keep_japanese_and_localize_english() {
        for (kind, japanese, english) in [
            ("tool_result", "shell結果", "Execution result"),
            ("event", "状態変更", "State change"),
            (
                "interrupted_run",
                "前回処理の中断情報",
                "Previous work was interrupted",
            ),
            (
                "future-kind",
                "未対応の項目: future-kind",
                "Unsupported item: future-kind",
            ),
        ] {
            let (projected, _) = initial(kind, &serde_json::json!({}));
            assert_eq!(projected.title, japanese);
            assert_eq!(projected.ui_messages.title.unwrap().render(En), english);
        }
        let body = "### Part 1\nkept text\n\nShellboy source nodes: node-a";
        let payload = serde_json::json!({
            "summary_format":"shellboy-summary-v1",
            "summary_part_keys":["part-a"],
            "source_node_ids":["node-a"],
            "item":{"content":[{"type":"input_text", "text":body}]}
        });
        let (mut projected, _) = initial("summary", &payload);
        assert!(summary_projection(&mut projected, &payload).is_some());
        assert_eq!(projected.title, "要約 | 置換1件 | 出典1件 | F6>O 原文");
        assert_eq!(
            projected.ui_messages.title.unwrap().render(En),
            "Summary | 1 replaced | 1 source | F6>O Original"
        );
        assert_eq!(payload["item"]["content"][0]["text"], body);
    }
    #[test]
    fn generated_summary_count_title_has_bilingual_projection() {
        let payload = serde_json::json!({"source_node_ids":["node-a", "node-b"]});
        let (mut projected, _) = initial("summary", &payload);
        summary_projection(&mut projected, &payload);
        let title = projected.ui_messages.title.as_ref().unwrap();
        assert_eq!(projected.title, "要約 | 出典2件 | F6>O 原文");
        assert_eq!(title.render(Ja), projected.title);
        assert_eq!(title.render(En), "Summary | 2 sources | F6>O Original");
    }
}
