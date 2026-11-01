//! Retained projection limits; window admission stays before allocation.
use super::*;

pub(super) fn markdown(p: &mut NodePresentation) {
    let mut markdown_bytes = 0;
    for b in &mut p.blocks {
        let bytes = b
            .markdown
            .iter()
            .map(|t| {
                size_of::<MarkdownToken>()
                    + t.text.len()
                    + match &t.kind {
                        MarkdownKind::Link(s) | MarkdownKind::CodeBlock(s) => s.len(),
                        _ => 0,
                    }
            })
            .sum::<usize>();
        markdown_bytes += bytes;
        if markdown_bytes > MAX_METADATA_BYTES {
            b.markdown = Vec::new();
            p.details_omitted = true;
            p.metadata_limited = true;
        }
    }
}

pub(super) fn finish(p: &mut NodePresentation, payload: &Value) {
    let mut total = 0;
    let before = p.fields.len();
    p.fields.retain(|f| {
        total += f.key.len() + f.label.len() + f.value.len() + size_of::<PresentationField>();
        total <= MAX_METADATA_BYTES
    });
    p.fields.truncate(MAX_FIELDS);
    p.links.truncate(MAX_LINKS);
    p.metadata_limited |= p.blocks.iter().any(|b| {
        matches!(b.kind, BlockKind::Prose | BlockKind::Reasoning)
            && !b.text.is_empty()
            && b.markdown.is_empty()
    }) || before != p.fields.len()
        || p.fields.len() >= MAX_FIELDS
        || p.fields.iter().any(|f| f.value_omitted)
        || p.blocks.len() >= MAX_BLOCKS
        || p.links.len() >= MAX_LINKS;
    p.details_omitted |= p.metadata_limited || p.body_omitted_chars > 0 || p.body_extent_unknown;
    fields::decorate(&mut p.fields, payload);
    // A bounded provenance cache is optional. If metadata is full, preserve all
    // canonical/legacy facts and mark the display as limited instead of cutting
    // a translated safety sentence or dropping an unrelated source field.
    if p.metadata_bytes() > MAX_METADATA_BYTES {
        p.ui_messages.clear();
        p.metadata_limited = true;
        p.details_omitted = true;
    }
    p.bound_metadata();
}

#[cfg(test)]
mod localized_metadata_tests {
    use super::*;
    use crate::presentation::messages::{Message, Text};

    #[test]
    fn combined_metadata_pressure_discards_optional_translation_without_cutting_source_facts() {
        let source = "自動での再実行はしません\nProvider RAW error 🙂 e\u{301}";
        let mut p = NodePresentation {
            title: "saved original".into(),
            blocks: vec![PresentationBlock {
                text: source.into(),
                kind: BlockKind::Event,
                ..Default::default()
            }],
            ..Default::default()
        };
        let source_before = p.blocks[0].clone();
        p.ui_messages.title = Some(Message::Text(Text::PreviousRecord));
        assert!(p.ui_messages.set_body(
            0,
            vec![
                Message::Text(Text::NoReplay),
                Message::Text(Text::ResultUncertain)
            ]
        ));
        p.fields = (0..MAX_FIELDS)
            .map(|n| PresentationField {
                key: format!("/padding/{n}"),
                label: String::new(),
                value: "x".repeat(600),
                display: None,
                value_omitted: false,
            })
            .collect();
        finish(&mut p, &serde_json::json!({}));
        assert!(p.metadata_bytes() <= MAX_METADATA_BYTES);
        assert!(p.ui_messages.title.is_none());
        assert!(p.ui_messages.bodies.is_empty());
        assert!(p.metadata_limited && p.details_omitted);
        assert_eq!(p.blocks[0], source_before);
    }

    #[test]
    fn provenance_admission_is_bounded_and_never_clips_a_sentence() {
        let mut p = NodePresentation::default();
        let sentence = Message::Raw {
            text: "e\u{301}".repeat(200),
        };
        let mut accepted = 0;
        for index in 0..128 {
            if p.ui_messages.set_body(index, vec![sentence.clone()]) {
                accepted += 1;
            }
            assert!(p.ui_messages.retained_bytes() <= 8 * 1024);
        }
        assert!(accepted > 0 && accepted < 128);
        for (_, messages) in &p.ui_messages.bodies {
            assert_eq!(messages, &[sentence.clone()]);
        }
    }
}
