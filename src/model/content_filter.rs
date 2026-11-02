//! Bounded opaque facts from provider filter containers; never a retry policy.
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_FACTS: usize = 32;
const MAX_VISITS: usize = 512;
const MAX_DEPTH: usize = 8;
const MAX_PATH: usize = 192;
const MAX_VALUE: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterFact {
    pub path: String,
    /// None is missing evidence; JSON null remains a provider-returned value.
    pub value: Option<Value>,
    pub recorded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentFilterFacts {
    pub reason: Option<Value>,
    pub reason_recorded: bool,
    pub facts: Vec<FilterFact>,
    pub limited: bool,
    pub next_actions: Vec<String>,
}

impl ContentFilterFacts {
    pub fn from_response(raw: &Value) -> Self {
        let mut result = Self::default();
        if let Some(reason) = raw.pointer("/incomplete_details/reason") {
            if reason.is_object() || reason.is_array() {
                result.limited = true;
            } else {
                result.reason_recorded = true;
                result.reason = Some(result.scalar(reason));
            }
        }
        if raw
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
            == Some("content_filter")
        {
            result.next_actions = [
                "retry_verdict_may_differ",
                "rephrase_and_resume",
                "remove_relevant_tool_output_or_detach_attachments",
                "copy_existing_files_with_shell_cp_or_curl_without_model_output",
            ]
            .map(str::to_owned)
            .into();
        }
        let mut visits = 0;
        result.scan(raw, "", 0, &mut visits);
        result
    }

    pub fn is_content_filter(&self) -> bool {
        self.reason.as_ref().and_then(Value::as_str) == Some("content_filter")
    }

    fn bounded(&mut self, text: &str, max: usize) -> String {
        let mut chars = text.chars();
        let result = chars.by_ref().take(max).collect();
        self.limited |= chars.next().is_some();
        result
    }

    fn scalar(&mut self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.bounded(text, MAX_VALUE)),
            Value::Array(_) | Value::Object(_) => {
                self.limited = true;
                Value::Null
            }
            _ => value.clone(),
        }
    }

    fn push(&mut self, path: &str, value: Option<&Value>) {
        if self.facts.len() >= MAX_FACTS {
            self.limited = true;
            return;
        }
        let path = self.bounded(path, MAX_PATH);
        let recorded = value.is_some();
        let value = value.map(|v| self.scalar(v));
        self.facts.push(FilterFact {
            path,
            value,
            recorded,
        });
    }

    fn enter(&mut self, depth: usize, visits: &mut usize) -> bool {
        if depth > MAX_DEPTH || *visits >= MAX_VISITS || self.facts.len() >= MAX_FACTS {
            self.limited = true;
            return false;
        }
        *visits += 1;
        true
    }

    fn path(&mut self, parent: &str, key: &str) -> String {
        let key = self
            .bounded(key, MAX_PATH)
            .replace('~', "~0")
            .replace('/', "~1");
        self.bounded(&format!("{parent}/{key}"), MAX_PATH)
    }

    fn verdicts(&mut self, value: &Value, path: &str, depth: usize, visits: &mut usize) {
        if !self.enter(depth, visits) {
            return;
        }
        match value {
            Value::Object(fields) => {
                let leaf =
                    fields.is_empty() || fields.values().any(|v| !v.is_object() && !v.is_array());
                for (key, value) in fields.iter().take(MAX_FACTS) {
                    let next = self.path(path, key);
                    self.verdicts(value, &next, depth + 1, visits);
                }
                self.limited |= fields.len() > MAX_FACTS;
                if leaf {
                    for key in ["filtered", "detected", "severity"] {
                        if !fields.contains_key(key) {
                            let next = self.path(path, key);
                            self.push(&next, None);
                        }
                    }
                }
            }
            Value::Array(items) => {
                for (index, value) in items.iter().take(MAX_FACTS).enumerate() {
                    let next = self.path(path, &index.to_string());
                    self.verdicts(value, &next, depth + 1, visits);
                }
                self.limited |= items.len() > MAX_FACTS;
            }
            _ => self.push(path, Some(value)),
        }
    }

    fn scan(&mut self, value: &Value, path: &str, depth: usize, visits: &mut usize) {
        if !self.enter(depth, visits) {
            return;
        }
        if let Some(fields) = value.as_object() {
            for key in [
                "content_filter_results",
                "content_filter_result",
                "prompt_filter_results",
            ] {
                if let Some(value) = fields.get(key) {
                    let next = self.path(path, key);
                    self.verdicts(value, &next, depth + 1, visits);
                }
            }
            // Inspect only response structure, never arbitrary provider/user text.
            for key in ["output", "content"] {
                if let Some(items) = fields.get(key).and_then(Value::as_array) {
                    let next = self.path(path, key);
                    for (index, item) in items.iter().take(MAX_FACTS).enumerate() {
                        let next = self.path(&next, &index.to_string());
                        self.scan(item, &next, depth + 1, visits);
                    }
                    self.limited |= items.len() > MAX_FACTS;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn opaque_verdicts_and_missing_values_are_distinct() {
        let raw = json!({"incomplete_details":{"reason":"content_filter"},"content_filter_results":{
            "protected_material_code":{"filtered":true,"detected":true,"provider_extension":"unfamiliar"},
            "violence":{"filtered":false,"severity":"safe","detected":null}
        }});
        let facts = ContentFilterFacts::from_response(&raw);
        assert!(facts.is_content_filter());
        assert!(!facts.limited);
        for (path, value) in [
            (
                "/content_filter_results/protected_material_code/filtered",
                Some(json!(true)),
            ),
            (
                "/content_filter_results/protected_material_code/severity",
                None,
            ),
            (
                "/content_filter_results/protected_material_code/provider_extension",
                Some(json!("unfamiliar")),
            ),
            (
                "/content_filter_results/violence/filtered",
                Some(json!(false)),
            ),
            (
                "/content_filter_results/violence/detected",
                Some(Value::Null),
            ),
        ] {
            assert!(facts.facts.contains(&FilterFact {
                path: path.into(),
                recorded: value.is_some(),
                value
            }));
        }
        assert_eq!(facts.next_actions.len(), 4);
        assert_eq!(
            ContentFilterFacts::from_response(&json!({})),
            ContentFilterFacts::default()
        );
    }

    #[test]
    fn nested_verdicts_are_bounded_without_mutating_source() {
        let raw = json!({"output":[{"content":[{"content_filter_results":{
            "unknown/category":{"filtered":"unknown-value", "severity":"x".repeat(100_000)}
        }}]}]});
        let before = raw.clone();
        let facts = ContentFilterFacts::from_response(&raw);
        assert!(facts.limited);
        assert!(
            facts
                .facts
                .iter()
                .any(|f| f.path.contains("unknown~1category"))
        );
        assert!(facts.facts.len() <= MAX_FACTS);
        assert!(serde_json::to_string(&facts).unwrap().len() < 4096);
        assert_eq!(raw, before);
        assert!(
            facts.next_actions.is_empty(),
            "no inference from category names"
        );
    }
}

pub const NEXT_ACTIONS_HINT: &str = "The provider filter stopped the response. Retry may receive a different verdict; rephrase and resume; remove relevant tool output or detach attachments with /detach. When copying existing files, shell cp or curl can copy them without having the model emit their body.";
