//! Current public shell stdin result contract.
use crate::protocol::*;
use serde_json::{Value, json};

#[test]
fn public_shell_status_records_allow_additive_fields() {
    let schema: Value =
        serde_json::from_str(include_str!("../docs/reference/protocol.schema.json")).unwrap();
    for name in ["ShellDetails", "WorkItem", "StatusSnapshot"] {
        assert_ne!(
            schema["definitions"][name]["additionalProperties"],
            json!(false)
        );
    }
}

#[test]
fn new_shell_record_defaults_missing_stdin_facts_to_unknown() {
    let details: ShellDetails = serde_json::from_value(
        json!({"execution_id":"s","run_id":"r","state":"running","output_complete":false}),
    )
    .unwrap();
    assert_eq!(details.stdin_open, None);
    assert_eq!(details.stdin_last_write_at, None);
}

#[test]
fn merged_typed_list_preserves_nullable_stdin_facts_and_pre283_missing_fields() {
    let original = json!({"execution_id":"s","run_id":"r","thread_id":"t","started_at":null,"ended_at":null,"state":"running","exit_code":null,"command_excerpt":null,"command_characters":0,"command_excerpt_redacted":false,"command_excerpt_truncated":false,"started_at_known":false,"future_extension":true});
    let missing: ShellListItem = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(missing.stdin_open, None);
    assert_eq!(missing.stdin_last_write_at, None);
    let missing = serde_json::to_value(missing).unwrap();
    assert_eq!(missing["stdin_open"], Value::Null);
    assert_eq!(missing["stdin_last_write_at"], Value::Null);
    let mut with_facts = original;
    with_facts["stdin_open"] = json!(false);
    with_facts["stdin_last_write_at"] = json!("2026-11-09T01:02:03Z");
    let typed: ShellListItem = serde_json::from_value(with_facts.clone()).unwrap();
    assert_eq!(typed.stdin_open, Some(false));
    assert_eq!(
        typed.stdin_last_write_at.as_deref(),
        Some("2026-11-09T01:02:03Z")
    );
    assert_eq!(
        serde_json::to_value(typed).unwrap()["future_extension"],
        true
    );
    let schema: Value = serde_json::from_str(&crate::protocol::schema_json()).unwrap();
    for name in ["ShellDetails", "ShellListItem", "WorkItem"] {
        for field in ["stdin_open", "stdin_last_write_at"] {
            assert!(
                schema["definitions"][name]["properties"]
                    .get(field)
                    .is_some(),
                "{name}/{field} must be a declared field, not just an extra"
            );
        }
    }
}
