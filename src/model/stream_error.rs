//! Narrow error-envelope recognition and embedded status provenance for SSE only.
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StreamProviderStatusSource {
    StreamErrorCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamProviderStatus {
    pub value: u16,
    pub source: StreamProviderStatusSource,
}

#[derive(Clone, Copy)]
enum Context {
    Root,
    Response,
    Error,
    Other,
}

pub(super) struct ParsedValue {
    pub(super) value: Value,
    pub(super) duplicate_relevant_key: bool,
    pub(super) untyped_error_candidate: bool,
    pub(super) ambiguous_output_keys: bool,
}
struct ValueSeed(Context);

impl<'de> DeserializeSeed<'de> for ValueSeed {
    type Value = ParsedValue;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(ValueVisitor(self.0))
    }
}

struct ValueVisitor(Context);
impl<'de> Visitor<'de> for ValueVisitor {
    type Value = ParsedValue;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(ParsedValue {
            value: Value::Bool(value),
            duplicate_relevant_key: false,
            untyped_error_candidate: false,
            ambiguous_output_keys: false,
        })
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(ParsedValue {
            value: Value::Number(value.into()),
            duplicate_relevant_key: false,
            untyped_error_candidate: false,
            ambiguous_output_keys: false,
        })
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(ParsedValue {
            value: Value::Number(value.into()),
            duplicate_relevant_key: false,
            untyped_error_candidate: false,
            ambiguous_output_keys: false,
        })
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        let number = Number::from_f64(value).ok_or_else(|| E::custom("non-finite JSON number"))?;
        Ok(ParsedValue {
            value: Value::Number(number),
            duplicate_relevant_key: false,
            untyped_error_candidate: false,
            ambiguous_output_keys: false,
        })
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(ParsedValue {
            value: Value::String(value.to_owned()),
            duplicate_relevant_key: false,
            untyped_error_candidate: false,
            ambiguous_output_keys: false,
        })
    }
    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(ParsedValue {
            value: Value::String(value),
            duplicate_relevant_key: false,
            untyped_error_candidate: false,
            ambiguous_output_keys: false,
        })
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(ParsedValue {
            value: Value::Null,
            duplicate_relevant_key: false,
            untyped_error_candidate: false,
            ambiguous_output_keys: false,
        })
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(ParsedValue {
            value: Value::Null,
            duplicate_relevant_key: false,
            untyped_error_candidate: false,
            ambiguous_output_keys: false,
        })
    }
    fn visit_some<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        ValueSeed(self.0).deserialize(deserializer)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        let mut duplicate = false;
        while let Some(value) = seq.next_element_seed(ValueSeed(Context::Other))? {
            duplicate |= value.duplicate_relevant_key;
            values.push(value.value);
        }
        Ok(ParsedValue {
            value: Value::Array(values),
            duplicate_relevant_key: duplicate,
            untyped_error_candidate: false,
            ambiguous_output_keys: false,
        })
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        let mut duplicate = false;
        let mut untyped_error_candidate = false;
        let mut ambiguous_output_keys = false;
        while let Some(key) = map.next_key::<String>()? {
            let relevant = match self.0 {
                Context::Root => matches!(key.as_str(), "type" | "error"),
                Context::Response => key == "error",
                Context::Error => key == "code",
                Context::Other => false,
            };
            let output_key = matches!(
                (self.0, key.as_str()),
                (Context::Root, "type" | "response") | (Context::Response, "output")
            );
            if (relevant || output_key) && values.contains_key(&key) {
                duplicate |= relevant;
                ambiguous_output_keys |= output_key;
            }
            let child_context = match (self.0, key.as_str()) {
                (Context::Root, "error") | (Context::Response, "error") => Context::Error,
                (Context::Root, "response") => Context::Response,
                _ => Context::Other,
            };
            let value = map.next_value_seed(ValueSeed(child_context))?;
            duplicate |= value.duplicate_relevant_key;
            ambiguous_output_keys |= value.ambiguous_output_keys;
            if matches!(self.0, Context::Root)
                && key == "error"
                && matches!(value.value, Value::Object(_) | Value::String(_))
            {
                untyped_error_candidate = true;
            }
            untyped_error_candidate |= value.untyped_error_candidate;
            values.insert(key, value.value);
        }
        Ok(ParsedValue {
            value: Value::Object(values),
            duplicate_relevant_key: duplicate,
            untyped_error_candidate,
            ambiguous_output_keys,
        })
    }
}

/// Parse a bounded SSE frame, retaining key ambiguity before values are overwritten.
pub(super) fn parse_frame(bytes: &[u8]) -> Result<ParsedValue, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let parsed = ValueSeed(Context::Root).deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(parsed)
}

pub(super) fn recognized_untyped_error(value: &Value) -> bool {
    value.get("type").is_none()
        && value
            .get("error")
            .is_some_and(|error| error.is_object() || error.is_string())
}

pub(super) fn status_from_error(
    error: &Value,
    duplicate_relevant_key: bool,
) -> Option<StreamProviderStatus> {
    if duplicate_relevant_key {
        return None;
    }
    let code = error.get("code").and_then(Value::as_str)?;
    let bytes = code.as_bytes();
    if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value = code.parse::<u16>().ok()?;
    (100..=599)
        .contains(&value)
        .then_some(StreamProviderStatus {
            value,
            source: StreamProviderStatusSource::StreamErrorCode,
        })
}

pub(super) fn status_from_event(
    value: &Value,
    duplicate_relevant_key: bool,
) -> Option<StreamProviderStatus> {
    let error = match value.get("type").and_then(Value::as_str) {
        Some("response.failed" | "response.incomplete") => value.get("response")?.get("error")?,
        _ => value.get("error")?,
    };
    status_from_error(error, duplicate_relevant_key)
}
