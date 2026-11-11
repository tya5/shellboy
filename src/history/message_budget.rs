//! Whole optional message fields are charged against the exact response envelope.
use crate::protocol::{MAX_FRAME_BYTES, MessageProvenance, Response};
use serde_json::Value;

// Only provenance.message is optional here; typed text and all other metadata stay intact.
pub(crate) fn fit(response: &mut Response<Value>) -> Result<(), serde_json::Error> {
    fn strip(value: &mut Value, saved: &mut Vec<Value>) {
        match value {
            Value::Object(object) => {
                if let Some(Value::Object(provenance)) = object.get_mut("provenance") {
                    saved.push(provenance.remove("message").unwrap_or(Value::Null));
                }
                for (key, child) in object.iter_mut() {
                    if key != "provenance" {
                        strip(child, saved);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    strip(item, saved);
                }
            }
            _ => {}
        }
    }
    fn restore(value: &mut Value, saved: &mut impl Iterator<Item = Value>, used: &mut usize) {
        match value {
            Value::Object(object) => {
                if let Some(Value::Object(provenance)) = object.get_mut("provenance") {
                    if let Some(message) = saved.next().filter(|m| !m.is_null()) {
                        if serde_json::from_value::<MessageProvenance>(message.clone()).is_ok() {
                            let bytes = serde_json::to_vec(&message).expect("JSON value").len();
                            let property =
                                serde_json::to_vec("message").expect("JSON key").len() + 1;
                            let charge = property + bytes + usize::from(!provenance.is_empty());
                            if bytes <= 4096
                                && used
                                    .checked_add(charge)
                                    .is_some_and(|n| n <= MAX_FRAME_BYTES)
                            {
                                *used += charge;
                                provenance.insert("message".into(), message);
                            }
                        }
                    }
                }
                for (key, child) in object.iter_mut() {
                    if key != "provenance" {
                        restore(child, saved, used);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    restore(item, saved, used);
                }
            }
            _ => {}
        }
    }
    let mut saved = Vec::new();
    if let Some(result) = response.result.as_mut() {
        strip(result, &mut saved);
    }
    let mut used = serde_json::to_vec(response)?.len();
    if let Some(result) = response.result.as_mut() {
        restore(result, &mut saved.into_iter(), &mut used);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
            fit(&mut value).unwrap();
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
