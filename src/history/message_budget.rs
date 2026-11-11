//! Whole optional message fields are charged against the exact response envelope.
use crate::protocol::{MAX_FRAME_BYTES, MessageProvenance, Response};
use serde_json::Value;

// Only official optional message/notification slots may be omitted; text stays intact.
pub(crate) enum Slots {
    Body,
    Nodes,
}

pub(crate) fn fit(response: &mut Response<Value>, slots: Slots) -> Result<(), serde_json::Error> {
    // These paths are supplied by the operation's typed response boundary, never payload keys.
    // Nodes inherits history list/window's existing maximum of 200, owned by this response.
    if matches!(slots, Slots::Nodes)
        && response
            .result
            .as_ref()
            .and_then(|r| r.get("items"))
            .and_then(Value::as_array)
            .is_some_and(|items| items.len() > 200)
    {
        return Err(<serde_json::Error as serde::ser::Error>::custom(
            "history node count exceeds response bound",
        ));
    }
    let paths = match slots {
        Slots::Body => vec!["/provenance".to_owned()],
        Slots::Nodes => (0..response
            .result
            .as_ref()
            .and_then(|r| r.get("items"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len))
            .map(|i| format!("/items/{i}/provenance"))
            .collect(),
    };
    let mut saved = Vec::with_capacity(paths.len() * 2);
    for path in &paths {
        for field in ["message", "notification"] {
            let value = response
                .result
                .as_mut()
                .and_then(|r| r.pointer_mut(path))
                .and_then(Value::as_object_mut)
                .and_then(|p| p.remove(field));
            saved.push((path, field, value));
        }
    }
    let mut used = serde_json::to_vec(response)?.len();
    for (path, field, value) in saved {
        let Some(value) = value else { continue };
        let bytes = serde_json::to_vec(&value)?;
        let valid = match field {
            "message" => serde_json::from_value::<MessageProvenance>(value.clone())
                .ok()
                .is_some_and(|relation| bytes.len() <= 4096 && valid(&value, &relation)),
            "notification" => {
                bytes.len() <= 2048
                    && std::str::from_utf8(&bytes)
                        .ok()
                        .and_then(super::notification::decode)
                        .is_some()
            }
            _ => false,
        };
        if !valid {
            continue;
        }
        let Some(provenance) = response
            .result
            .as_mut()
            .and_then(|r| r.pointer_mut(path))
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        let charge = serde_json::to_vec(field)?.len()
            + 1
            + bytes.len()
            + usize::from(!provenance.is_empty());
        if used
            .checked_add(charge)
            .is_some_and(|total| total <= MAX_FRAME_BYTES)
        {
            used += charge;
            provenance.insert(field.into(), value);
        }
    }
    Ok(())
}
fn valid(value: &Value, relation: &MessageProvenance) -> bool {
    use crate::protocol::MessageReplyState;
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.keys().any(|k| {
        !matches!(
            k.as_str(),
            "expect_reply" | "reply_to" | "reply_state" | "reply_node_id"
        )
    }) {
        return false;
    }
    if object.values().any(Value::is_null) {
        return false;
    }
    let id = |s: &str| !s.trim().is_empty() && s.len() <= 256;
    if !relation.reply_to.as_deref().is_none_or(id)
        || !relation.reply_node_id.as_deref().is_none_or(id)
    {
        return false;
    }
    if relation.reply_to.is_some() {
        return !relation.expect_reply
            && relation.reply_state.is_none()
            && relation.reply_node_id.is_none();
    }
    if !relation.expect_reply {
        return relation.reply_state.is_none() && relation.reply_node_id.is_none();
    }
    matches!(
        (&relation.reply_state, &relation.reply_node_id),
        (Some(MessageReplyState::Replied), Some(_))
            | (
                Some(
                    MessageReplyState::Pending
                        | MessageReplyState::Ignored
                        | MessageReplyState::Cancelled
                ),
                None
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn relation_states_exclusive_targets_and_utf8_ids_are_strict() {
        for message in [
            json!({"expect_reply":false,"reply_to":"a","reply_node_id":"b"}),
            json!({"expect_reply":true,"reply_to":"a","reply_state":"replied","reply_node_id":"b"}),
            json!({"expect_reply":true,"reply_state":"pending","reply_node_id":"b"}),
            json!({"expect_reply":false,"reply_to":null}),
            json!({"expect_reply":false,"reply_to":"\u{e9}".repeat(129)}),
        ] {
            let mut response = Response::success(
                "op",
                json!({"provenance":{"kind":"user","message":message},"text":"unchanged"}),
            );
            fit(&mut response, Slots::Body).unwrap();
            assert!(
                response.result.as_ref().unwrap()["provenance"]
                    .get("message")
                    .is_none()
            );
            assert_eq!(response.result.as_ref().unwrap()["text"], "unchanged");
        }
        let mut exact = Response::success(
            "op",
            json!({"provenance":{"message":{"expect_reply":false,"reply_to":"\u{e9}".repeat(128)}}}),
        );
        fit(&mut exact, Slots::Body).unwrap();
        assert!(
            exact.result.as_ref().unwrap()["provenance"]
                .get("message")
                .is_some()
        );
    }
    #[test]
    fn opaque_payload_provenance_is_never_visited() {
        let opaque = json!({"provenance":{"message":{"unknown":"private opaque content"}}});
        let mut response = Response::success(
            "op",
            json!({"provenance":{"message":{"expect_reply":false,"unknown":true}},"content":opaque}),
        );
        let before = serde_json::to_vec(&response.result.as_ref().unwrap()["content"]).unwrap();
        fit(&mut response, Slots::Body).unwrap();
        assert!(
            response.result.as_ref().unwrap()["provenance"]
                .get("message")
                .is_none()
        );
        assert_eq!(
            before,
            serde_json::to_vec(&response.result.as_ref().unwrap()["content"]).unwrap()
        );
        let mut page = Response::success(
            "op",
            json!({"items":[{"provenance":{"message":{"expect_reply":false,"reply_to":"x".repeat(257)}},"content":opaque}]}),
        );
        fit(&mut page, Slots::Nodes).unwrap();
        assert!(
            page.result.as_ref().unwrap()["items"][0]["provenance"]
                .get("message")
                .is_none()
        );
        assert_eq!(
            before,
            serde_json::to_vec(&page.result.as_ref().unwrap()["items"][0]["content"]).unwrap()
        );
    }
    #[test]
    fn exact_envelope_escaping_and_optional_property_boundary() {
        let mut response = Response::success(
            "operation\"\\escaped",
            json!({"provenance":{"kind":"user","message":{"expect_reply":false,"reply_to":"id\"\\"}},"padding":""}),
        );
        let message = response.result.as_ref().unwrap()["provenance"]["message"].clone();
        response.result.as_mut().unwrap()["provenance"]
            .as_object_mut()
            .unwrap()
            .remove("message");
        let base = serde_json::to_vec(&response).unwrap().len();
        let charge = serde_json::to_vec(&message).unwrap().len()
            + serde_json::to_vec("message").unwrap().len()
            + 2;
        for delta in [-1i64, 0, 1] {
            let mut value = response.clone();
            value.result.as_mut().unwrap()["padding"] = Value::String(
                "x".repeat((MAX_FRAME_BYTES - base - charge) + delta.max(0) as usize),
            );
            if delta < 0 {
                value.result.as_mut().unwrap()["padding"].as_str().unwrap();
                value.result.as_mut().unwrap()["padding"] =
                    Value::String("x".repeat(MAX_FRAME_BYTES - base - charge - 1));
            }
            value.result.as_mut().unwrap()["provenance"]["message"] = message.clone();
            fit(&mut value, Slots::Body).unwrap();
            assert_eq!(
                value.result.as_ref().unwrap()["provenance"]
                    .get("message")
                    .is_some(),
                delta <= 0
            );
            assert!(serde_json::to_vec(&value).unwrap().len() <= MAX_FRAME_BYTES);
        }
    }
}
