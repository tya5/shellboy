//! Responses wire adapter. Shellboy's canonical conversation is already in this shape, so both directions pass through.
use super::*;

pub(super) fn encode(request: Value) -> Result<Value, ModelError> {
    Ok(request)
}

pub(super) fn decode_json(raw: Value) -> Result<Value, ModelError> {
    Ok(raw)
}
