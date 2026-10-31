//! Bounded model-authored labels; these never authorize or change commands.
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};
pub const MAX_CHARS: usize = 80;

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d).map(|s| s.map(|s| s.chars().take(MAX_CHARS).collect()))
}
pub(crate) fn label(raw: Option<&str>) -> Option<String> {
    let safe = crate::presentation::sanitize_plain(raw?, 0)
        .0
        .replace(['\n', '\r'], " ");
    let safe = safe.trim();
    (!safe.is_empty()).then(|| safe.chars().take(MAX_CHARS).collect())
}
/// Annotate the response envelope, not API output items or executable arguments.
pub(crate) fn normalize(raw: &mut Value, argument_limit: usize) {
    if let Some(object) = raw.as_object_mut() {
        object.remove("_shellboy_description_truncations");
    }
    let mut shortened = Map::new();
    if let Some(items) = raw.get_mut("output").and_then(Value::as_array_mut) {
        for item in items {
            if item["type"] != "function_call" || item["name"] != "shell" {
                continue;
            }
            let Some(text) = item["arguments"]
                .as_str()
                .filter(|s| s.len() <= argument_limit)
            else {
                continue;
            };
            let Ok(mut args) = serde_json::from_str::<Value>(text) else {
                continue;
            };
            let Some(description) = args["description"].as_str() else {
                continue;
            };
            let count = description.chars().count();
            if count <= MAX_CHARS {
                continue;
            }
            let bounded: String = description.chars().take(MAX_CHARS).collect();
            args["description"] = Value::String(bounded);
            shortened.insert(
                item["call_id"].as_str().unwrap_or("").into(),
                Value::from(count),
            );
            item["arguments"] = Value::String(args.to_string());
        }
    }
    if !shortened.is_empty() {
        raw["_shellboy_description_truncations"] = Value::Object(shortened);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn bounded_unicode_arguments_keep_command_exact_and_record_actual_truncation() {
        let command = "printf 'unchanged command'";
        let mut raw = json!({"output":[{"type":"function_call","name":"shell","call_id":"c","arguments":json!({"command":command,"description":"語".repeat(90)}).to_string()}]});
        normalize(&mut raw, 1_000_000);
        let args: Value =
            serde_json::from_str(raw["output"][0]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["command"], command);
        assert_eq!(args["description"].as_str().unwrap().chars().count(), 80);
        assert_eq!(raw["_shellboy_description_truncations"]["c"], 90);
        for value in [None, Some(""), Some(" \t\r\n"), Some("\u{1b}\u{7}")] {
            assert!(label(value).is_none());
        }
        assert_eq!(
            label(Some("  inspect\nfiles\u{7} ")),
            Some("inspect files".into())
        );
    }
    #[test]
    fn provider_cannot_claim_a_truncation_and_absent_description_is_compatible() {
        let mut raw = json!({"_shellboy_description_truncations":{"forged":100},"output":[]});
        normalize(&mut raw, 10);
        assert!(raw.get("_shellboy_description_truncations").is_none());
        let args: crate::protocol::ShellToolArgs =
            serde_json::from_value(json!({"command":"true"})).unwrap();
        assert!(args.description.is_none());
    }
}

#[cfg(test)]
mod request_tests {
    #[test]
    fn opt_out_removes_parameter_from_exact_generation_request_without_disabling_shell() {
        let mut request = crate::model::response_request("fixture", vec![], None, true);
        assert_eq!(
            request["tools"][0]["parameters"]["properties"]["description"]["maxLength"],
            80
        );
        crate::model::configure_shell_description(&mut request, false);
        assert!(
            request["tools"][0]["parameters"]["properties"]
                .get("description")
                .is_none()
        );
        assert_eq!(request["tools"][0]["name"], "shell");
        assert!(
            request["tools"][0]["parameters"]["properties"]
                .get("command")
                .is_some()
        );
        assert!(crate::config::Config::default().shell.describe_calls);
        assert!(
            !crate::config::Config::parse("[shell]\ndescribe_calls=false\n")
                .unwrap()
                .shell
                .describe_calls
        );
    }
}
