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
    PATHS
        .map(|path| {
            format!(
                "CASE WHEN json_valid({column}) THEN {} END",
                sql_valid_integer(column, path)
            )
        })
        .join(",")
}

/// The caller supplies SQLite-produced JSON/JSONB or NULL, never unchecked text.
pub(crate) fn sql_valid_integer(column: &str, path: &str) -> String {
    format!(
        "CASE WHEN json_type({column},'{path}')='integer' AND typeof(json_extract({column},'{path}'))='integer' AND json_extract({column},'{path}')>=0 THEN json_extract({column},'{path}') END"
    )
}

/// Only the provider response envelope; never traverse output/user content.
pub(crate) fn normalize_response(response: &mut Value) {
    if let Some(usage) = response.get_mut("usage") {
        *usage = StandardUsage::from_value(usage).to_value();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;

    #[test]
    fn legacy_sql_and_save_validation_agree_at_every_numeric_boundary() {
        let db = Connection::open_in_memory().unwrap();
        let sql = format!("SELECT {}", sql_projection("?1"));
        for (literal, expected) in [
            ("0", Some(0)),
            ("123", Some(123)),
            ("9223372036854775807", Some(i64::MAX as u64)),
            ("9223372036854775808", None),
            ("18446744073709551615", None),
            ("18446744073709551616", None),
            ("-1", None),
            ("1.0", None),
            ("1e0", None),
            ("true", None),
            ("false", None),
            ("\"1\"", None),
            ("null", None),
            ("{}", None),
            ("[]", None),
        ] {
            let raw = format!(
                r#"{{"input_tokens":{literal},"input_tokens_details":{{"cached_tokens":{literal}}},"output_tokens":{literal},"output_tokens_details":{{"reasoning_tokens":{literal}}},"total_tokens":{literal}}}"#
            );
            let rust = StandardUsage::from_value(&serde_json::from_str(&raw).unwrap());
            let expected = StandardUsage {
                input_tokens: expected,
                cached_tokens: expected,
                output_tokens: expected,
                reasoning_tokens: expected,
                total_tokens: expected,
            };
            assert_eq!(rust, expected, "save: {literal}");
            let read = db
                .query_row(&sql, [&raw], |row| StandardUsage::read(row, 0))
                .unwrap();
            assert_eq!(read, expected, "SQL: {literal}");
            assert_eq!(StandardUsage::from_value(&rust.to_value()), expected);
        }
        for raw in [
            None,
            Some("{"),
            Some("not JSON"),
            Some("null"),
            Some("{}"),
            Some(r#"{"input_tokens_details":[],"output_tokens_details":"bad"}"#),
        ] {
            assert_eq!(
                db.query_row(&sql, [raw], |row| StandardUsage::read(row, 0))
                    .unwrap(),
                StandardUsage::default(),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn whitelist_discards_huge_appendices_but_never_recurses_into_content() {
        let mut raw = json!({"id":"fixed","usage":{
            "input_tokens":20,"input_tokens_details":{"cached_tokens":0,"extra":"x".repeat(128*1024)},
            "output_tokens":5,"output_tokens_details":{"reasoning_tokens":2,"extra":123},
            "total_tokens":17,"attribution":{"items":["x".repeat(128*1024)]}},
            "output":[{"usage":{"attribution":"user content"}}],"error":{"message":"preserved"}});
        let original = raw.clone();
        normalize_response(&mut raw);
        assert_eq!(
            raw["usage"],
            json!({"input_tokens":20,"input_tokens_details":{"cached_tokens":0},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":17})
        );
        assert_eq!(raw["output"], original["output"]);
        assert_eq!(raw["error"], original["error"]);
        assert_eq!(raw["id"], original["id"]);
        normalize_response(&mut raw);
        assert!(raw["usage"].to_string().len() < 256);
        let mixed = StandardUsage::from_value(
            &json!({"input_tokens":-1,"input_tokens_details":{"cached_tokens":0},"output_tokens":5,"total_tokens":null}),
        );
        assert_eq!(mixed.input_tokens, None);
        assert_eq!(mixed.cached_tokens, Some(0));
        assert_eq!(mixed.output_tokens, Some(5));
        assert_eq!(mixed.total_tokens, None);
        assert_ne!(mixed, StandardUsage::default());
        assert_eq!(StandardUsage::default().to_value(), json!({}));
    }
}
