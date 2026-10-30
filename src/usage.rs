//! Provider usage numbers shared by persistence and bounded scalar readers.
use rusqlite::Row;
use serde_json::{Map, Value};

const PATHS: [&str; 5] = [
    "$.input_tokens",
    "$.input_tokens_details.cached_tokens",
    "$.output_tokens",
    "$.output_tokens_details.reasoning_tokens",
    "$.total_tokens",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StandardUsage {
    pub input_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

impl StandardUsage {
    pub(crate) fn from_value(value: &Value) -> Self {
        let number = |pointer| {
            value
                .pointer(pointer)
                .and_then(Value::as_u64)
                .filter(|n| *n <= i64::MAX as u64)
        };
        Self {
            input_tokens: number("/input_tokens"),
            cached_tokens: number("/input_tokens_details/cached_tokens"),
            output_tokens: number("/output_tokens"),
            reasoning_tokens: number("/output_tokens_details/reasoning_tokens"),
            total_tokens: number("/total_tokens"),
        }
    }

    pub(crate) fn to_value(self) -> Value {
        let mut object = Map::new();
        for (key, count) in [
            ("input_tokens", self.input_tokens),
            ("output_tokens", self.output_tokens),
            ("total_tokens", self.total_tokens),
        ] {
            if let Some(count) = count {
                object.insert(key.into(), count.into());
            }
        }
        for (key, field, count) in [
            ("input_tokens_details", "cached_tokens", self.cached_tokens),
            (
                "output_tokens_details",
                "reasoning_tokens",
                self.reasoning_tokens,
            ),
        ] {
            if let Some(count) = count {
                object.insert(key.into(), serde_json::json!({field: count}));
            }
        }
        Value::Object(object)
    }

    pub(crate) fn read(row: &Row<'_>, start: usize) -> rusqlite::Result<Self> {
        let number = |index| -> rusqlite::Result<Option<u64>> {
            Ok(row.get::<_, Option<i64>>(index)?.map(|n| n as u64))
        };
        Ok(Self {
            input_tokens: number(start)?,
            cached_tokens: number(start + 1)?,
            output_tokens: number(start + 2)?,
            reasoning_tokens: number(start + 3)?,
            total_tokens: number(start + 4)?,
        })
    }
}

// SQL identifiers are internal constants, never caller input. Guard parsing in
// CASE, not WHERE: query planning must not expose malformed legacy documents.
pub(crate) fn sql_projection(column: &str) -> String {
    PATHS.map(|path| {
        format!("CASE WHEN json_valid({column}) THEN CASE WHEN json_type({column},'{path}')='integer' AND typeof(json_extract({column},'{path}'))='integer' AND json_extract({column},'{path}')>=0 THEN json_extract({column},'{path}') END END")
    }).join(",")
}

/// Only the provider response envelope; never traverse output/user content.
pub(crate) fn normalize_response(response: &mut Value) {
    if let Some(usage) = response.get_mut("usage") {
        *usage = StandardUsage::from_value(usage).to_value();
    }
}
