//! Bounded model-authored labels; these never authorize or change commands.
use serde::{Deserialize, Deserializer};
pub const MAX_CHARS: usize = 80;

#[derive(schemars::JsonSchema)]
#[schemars(transparent)]
#[allow(dead_code)]
pub(crate) struct DescriptionSchema(#[schemars(length(max = 80))] String);

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d).map(|s| s.map(|s| s.chars().take(MAX_CHARS).collect()))
}
pub(crate) fn label(raw: Option<&str>) -> Option<String> {
    let safe = crate::presentation::sanitize_plain(raw?, 0)
        .0
        .replace(['\n', '\r', '\t', '\u{2028}', '\u{2029}'], " ");
    let safe = safe.trim();
    (!safe.is_empty()).then(|| safe.chars().take(MAX_CHARS).collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn execution_and_label_are_bounded_without_changing_received_arguments() {
        let command = "echo unchanged";
        let arguments = format!(
            r#" {{ "description" : {}, "command" : {} }} "#,
            json!("語".repeat(100)),
            json!(command)
        );
        let output =
            json!([{"type":"function_call","name":"shell","call_id":"c","arguments":arguments}]);
        let response = crate::model::ModelResponse::completed(
            json!({"status":"completed","output":output.clone()}),
            true,
        )
        .unwrap();
        assert_eq!(response.output, output.as_array().unwrap().clone());
        assert_eq!(response.raw["output"], output);
        let args = response.calls[0].args.as_ref().unwrap();
        assert_eq!(args.command, command);
        assert_eq!(
            args.description.as_ref().unwrap().chars().count(),
            MAX_CHARS
        );
        assert_eq!(
            label(Some(&"語".repeat(100))).unwrap().chars().count(),
            MAX_CHARS
        );
    }
    #[test]
    fn absent_or_empty_description_falls_back_without_response_metadata() {
        let args: crate::protocol::ShellToolArgs =
            serde_json::from_value(json!({"command":"true"})).unwrap();
        assert!(args.description.is_none());
        for raw in [
            json!({"command":"true"}),
            json!({"command":"true","description":""}),
        ] {
            let args: crate::protocol::ShellToolArgs = serde_json::from_value(raw).unwrap();
            assert_eq!(args.command, "true");
            assert!(label(args.description.as_deref()).is_none());
        }
        for value in [None, Some(""), Some(" \t\r\n"), Some("\u{1b}\u{7}")] {
            assert!(label(value).is_none());
        }
        assert_eq!(
            label(Some("  inspect\nfiles\u{7} ")),
            Some("inspect files".into())
        );
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
        assert_eq!(
            request["tools"][0]["parameters"]["required"],
            serde_json::json!(["command", "description"])
        );
        let mut required_only = request.clone();
        required_only["tools"][0]["parameters"]["required"] = serde_json::json!(["command"]);
        assert_eq!(
            serde_json::to_vec(&request).unwrap().len()
                - serde_json::to_vec(&required_only).unwrap().len(),
            14
        );
        crate::model::configure_shell_description(&mut request, false);
        assert_eq!(
            request["tools"][0]["parameters"]["required"],
            serde_json::json!(["command"])
        );
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
