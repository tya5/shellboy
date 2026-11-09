//! Frozen pre-#283 public decoder definitions from main 4dc8f66.
#![allow(dead_code)]
use crate::protocol::*;
use crate::status::ResponseNodeRef;
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Deserialize)]
struct OldShellDetails {
    pub execution_id: String,
    pub run_id: String,
    pub state: ShellState,
    pub output_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default)]
    pub last_output_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_cleanup_confirmed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}
#[derive(Deserialize)]
struct OldWorkItem {
    pub kind: String,
    pub id: String,
    pub state: String,
    pub thread_id: String,
    pub thread_title: String,
    pub parent_thread_id: Option<String>,
    pub current_connection: String,
    pub current_model: String,
    pub run_id: Option<String>,
    pub owner_run_id: Option<String>,
    pub turn_id: Option<String>,
    pub execution_id: Option<String>,
    pub message_id: Option<String>,
    pub attempt_id: Option<String>,
    pub operation_id: Option<String>,
    pub end_message_node_id: Option<String>,
    pub response_nodes: Vec<ResponseNodeRef>,
    pub response_nodes_truncated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expect_reply: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub updated_at: Option<String>,
    pub activity_at: String,
    /// Read-time duration for live shells; saved duration for terminal shells.
    pub elapsed_ms: Option<u64>,
    pub last_output_at: Option<String>,
    pub attempt_connection: Option<String>,
    pub attempt_model: Option<String>,
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revisit: Option<RevisitSummary>,
    pub targets: Vec<Command>,
}
#[test]
fn stdin_additions_are_ignored_by_real_old_public_record_decoders() {
    let shell = json!({"execution_id":"s","run_id":"r","state":"running","output_complete":false,"stdin_open":true,"stdin_last_write_at":"2026-11-09T01:02:03Z"});
    let old: OldShellDetails = serde_json::from_value(shell).unwrap();
    assert_eq!(old.execution_id, "s");
    let item = json!({"kind":"shell","id":"s","state":"running","thread_id":"t","thread_title":"title","current_connection":"local","current_model":"synthetic","response_nodes":[],"response_nodes_truncated":false,"activity_at":"2026-11-09T00:00:00Z","targets":[],"stdin_open":false,"stdin_last_write_at":null});
    let old: OldWorkItem = serde_json::from_value(item).unwrap();
    assert_eq!(old.id, "s");
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
