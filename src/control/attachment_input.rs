//! Bind each current attachment version at its immutable source content slot.
use crate::model::wire_bytes::{Category, ItemTag};
use crate::{
    attachments::PreparedAttachments,
    protocol::{ApiError, ErrorCode},
};
use serde_json::{Value, json};
use std::collections::HashSet;

fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidState, message)
}

/// Local provenance fields never become unsupported API properties.
pub(crate) fn strip_local(item: &mut Value) {
    if let Some(object) = item.as_object_mut() {
        object.remove("attachments");
        object.remove("attachment_origin");
        object.remove("attachment_operation_id");
        object.remove("attachment_operation");
    }
}
/// Whether a replayed item has attachment content bound only at send time.
pub(crate) fn has_attachment_ref(item: &Value) -> bool {
    item.get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| {
            content
                .iter()
                .any(|part| part.get("type").and_then(Value::as_str) == Some("attachment_ref"))
        })
}

/// Append the replayed items with attachment content bound, and one source
/// tag per appended item (#186). Expanded attachment parts are `attachment`.
pub(super) fn bind(
    replay: &super::replay::ReplayLease,
    prepared: &mut PreparedAttachments,
    input: &mut Vec<Value>,
    tags: &mut Vec<ItemTag>,
) -> Result<(), ApiError> {
    let mut seen = HashSet::new();
    for (source, category, original) in replay.sourced_items() {
        let mut item = original.clone();
        let mut tag = ItemTag::new(category);
        if let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) {
            let mut expanded = Vec::new();
            for (element, part) in std::mem::take(content).into_iter().enumerate() {
                if part.get("type").and_then(Value::as_str) != Some("attachment_ref") {
                    expanded.push(part);
                    continue;
                }
                let id = part
                    .get("attachment_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("Attachment source element has no attachment ID"))?;
                if !seen.insert(id.to_owned()) {
                    return Err(invalid(
                        "Attachment reference occurs more than once in API input",
                    ));
                }
                let placement = prepared
                    .placements
                    .iter()
                    .find(|p| p.attachment_id == id)
                    .ok_or_else(|| {
                        invalid("Attachment source element has no matching prepared selection")
                    })?;
                if placement.source_node_id != source || placement.source_element_index != element {
                    return Err(invalid(
                        "Attachment source node or ordered element does not match its selection",
                    ));
                }
                if original.get("role").and_then(Value::as_str) != Some("user") {
                    return Err(invalid(
                        "Attachment source must convert to a user-role message with explicit provenance",
                    ));
                }
                let manifest = prepared
                    .manifest
                    .iter_mut()
                    .find(|m| m.attachment_id == id)
                    .ok_or_else(|| invalid("Attachment source has no request manifest"))?;
                manifest.api_item_index = Some(input.len());
                manifest.api_content_index = Some(expanded.len());
                manifest.api_content_count = placement.content.len();
                tag.parts.extend(
                    (expanded.len()..expanded.len() + placement.content.len())
                        .map(|part| (part, Category::Attachment)),
                );
                expanded.extend(placement.content.iter().cloned());
            }
            *content = expanded;
        }
        strip_local(&mut item);
        input.push(item);
        tags.push(tag);
    }
    // Replaced sources are absent from replay; report their excluded state at
    // this safe boundary instead of inventing a source message or binary.
    let mut detached_content = Vec::new();
    for placement in prepared
        .placements
        .iter()
        .filter(|p| p.detached && !seen.contains(&p.attachment_id))
    {
        let manifest = prepared
            .manifest
            .iter_mut()
            .find(|m| m.attachment_id == placement.attachment_id)
            .ok_or_else(|| invalid("Detached attachment notice has no manifest"))?;
        if manifest.state != "excluded" {
            return Err(invalid(
                "Only an excluded context replacement can be detached from its source",
            ));
        }
        manifest.api_item_index = Some(input.len());
        manifest.api_content_index = Some(detached_content.len());
        manifest.api_content_count = placement.content.len();
        detached_content.extend(placement.content.iter().cloned());
    }
    if !detached_content.is_empty() {
        input.push(json!({"role":"developer","content":detached_content}));
        tags.push(ItemTag::new(Category::Attachment));
    }
    if prepared
        .placements
        .iter()
        .any(|p| !p.detached && !seen.contains(&p.attachment_id))
    {
        return Err(invalid(
            "Prepared attachment is missing from its original API source message",
        ));
    }
    let updates: Vec<_> = prepared.manifest.iter().filter(|m| m.changed).map(|m| {
        json!({"attachment_id":m.attachment_id,"source_node_id":m.source_node_id,
            "previous_hash":m.previous_hash,"content_hash":m.content_hash,
            "update_id":format!("{}:{}:{}",m.attachment_id,m.previous_hash.as_deref().unwrap_or(""),m.content_hash.as_deref().unwrap_or(""))})
    }).collect();
    if !updates.is_empty() {
        input.push(json!({"role":"developer","content":[{"type":"input_text",
            "text":format!("Shellboy attachment updates v1: {}",json!(updates))}]}));
        tags.push(ItemTag::new(Category::Notice));
    }
    Ok(())
}
