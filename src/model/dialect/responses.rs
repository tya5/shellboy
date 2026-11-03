//! Responses wire adapter. Shellboy's canonical conversation is already in this shape, so both directions pass through.
use super::*;

pub(super) fn encode(
    mut request: Value,
    tags: Option<&[ItemTag]>,
) -> Result<(Value, Labels), ModelError> {
    let mut labels = Labels::new();
    // Opaque provider blocks belong to a specific provider dialect; none of
    // them is Responses, so none is sent here. Everything else passes through.
    if let Some(input) = request["input"].as_array_mut() {
        let mut kept = 0;
        let mut index = 0;
        input.retain(|item| {
            let keep = !crate::model::opaque::is_opaque(item);
            if keep && let Some(tag) = tags.and_then(|tags| tags.get(index)) {
                labels.push((
                    format!("/input/{kept}"),
                    wire_bytes::Label::Whole(tag.category),
                ));
                let parts = item["content"].as_array().map_or(0, Vec::len);
                for (part, category) in &tag.parts {
                    if *part < parts {
                        labels.push((
                            format!("/input/{kept}/content/{part}"),
                            wire_bytes::Label::Whole(*category),
                        ));
                    }
                }
            }
            kept += usize::from(keep);
            index += 1;
            keep
        });
    }
    Ok((request, labels))
}

pub(super) fn decode_json(raw: Value) -> Result<Value, ModelError> {
    Ok(raw)
}
