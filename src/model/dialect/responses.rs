//! Responses wire adapter. Shellboy's canonical conversation is already in this shape, so both directions pass through.
use super::*;

pub(super) fn encode(mut request: Value) -> Result<Value, ModelError> {
    // Opaque provider blocks belong to a specific provider dialect; none of
    // them is Responses, so none is sent here. Everything else passes through.
    if let Some(input) = request["input"].as_array_mut() {
        input.retain(|item| !crate::model::opaque::is_opaque(item));
    }
    Ok(request)
}

pub(super) fn decode_json(raw: Value) -> Result<Value, ModelError> {
    Ok(raw)
}
