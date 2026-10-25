//! Decoder for a known saved summary format, never a generic text/UUID stripper.
use serde_json::Value;
use std::collections::HashSet;

pub(super) struct Part<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}
pub(super) struct Decoded<'a> {
    pub body: &'a str,
    pub parts: Vec<Part<'a>>,
}
pub(super) fn decode(payload: &Value) -> Option<Decoded<'_>> {
    if payload.get("summary_format")?.as_str()? != "shellboy-summary-v1" {
        return None;
    }
    let sources = payload.get("source_node_ids")?.as_array()?;
    let keys = payload.get("summary_part_keys")?.as_array()?;
    if sources.is_empty()
        || sources.len() > 4096
        || keys.is_empty()
        || keys.len() > 128
        || keys.iter().any(|key| !key.is_string())
    {
        return None;
    }
    let sources = sources
        .iter()
        .map(Value::as_str)
        .collect::<Option<HashSet<_>>>()?;
    if sources
        .iter()
        .any(|id| id.is_empty() || id.contains([',', '\n']))
    {
        return None;
    }
    let content = payload.pointer("/item/content")?.as_array()?;
    if content.len() != 1 || content[0].get("type")?.as_str()? != "input_text" {
        return None;
    }
    let body = content[0].get("text")?.as_str()?;
    let mut leaves = 0;
    let mut same = true;
    crate::history::payload::visit(payload, |leaf| {
        leaves += 1;
        same &= leaf.text == body;
    });
    if leaves != 1 || !same {
        return None;
    }
    let mut parts = Vec::new();
    let mut rest = body;
    let mut char_offset = 0;
    let mut observed = HashSet::new();
    for index in 0..keys.len() {
        let prefix = format!("### Part {}\n", index + 1);
        if !rest.starts_with(&prefix) {
            return None;
        }
        let (segment, next) = if index + 1 < keys.len() {
            let boundary = format!("\n\n### Part {}\n", index + 2);
            let mut matches = rest.match_indices(&boundary);
            let (position, _) = matches.next()?;
            if matches.next().is_some() {
                return None;
            }
            (&rest[..position], &rest[position + 2..])
        } else {
            (rest, "")
        };
        let footer = "\n\nShellboy source nodes: ";
        let mut matches = segment.match_indices(footer);
        let (at, _) = matches.next()?;
        if matches.next().is_some() || at < prefix.len() {
            return None;
        }
        let ids = segment[at + footer.len()..].split(", ").collect::<Vec<_>>();
        if ids.is_empty() || ids.iter().any(|id| !sources.contains(id)) {
            return None;
        }
        observed.extend(ids);
        let text = &segment[prefix.len()..at];
        let start = char_offset + prefix.chars().count();
        parts.push(Part {
            text,
            start,
            end: start + text.chars().count(),
        });
        char_offset += segment.chars().count() + 2;
        rest = next;
    }
    if observed != sources {
        return None;
    }
    Some(Decoded { body, parts })
}
