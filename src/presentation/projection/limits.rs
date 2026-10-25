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
    p.bound_metadata();
}
