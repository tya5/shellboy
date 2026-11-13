//! Public retry-suppression reason permissions; no private memory key is serialized.
use super::discard;
use serde_json::{Value, json};

#[test]
fn retry_suppressed_is_preparation_only_and_has_no_provider_facts() {
    let permitted = json!({
        "phase":"closed_summary",
        "elapsed_ms":0,
        "outcome":{"kind":"failed","reason":"retry_suppressed","origin":"preparation"}
    });
    assert!(discard::decode(&permitted.to_string()).is_some());

    for forbidden in [
        json!({"origin":"provider_response"}),
        json!({"origin":"internal"}),
        json!({"origin":"preparation","http_status":401}),
        json!({"origin":"preparation","provider_status":400,"provider_status_source":"stream_error_code"}),
        json!({"origin":"preparation","final_group_count":1,"final_source_nodes":2}),
        json!({"origin":"preparation","provider_status_source":"stream_error_code"}),
    ] {
        let mut value = permitted.clone();
        value["outcome"]
            .as_object_mut()
            .unwrap()
            .extend(forbidden.as_object().unwrap().clone());
        assert!(discard::decode(&value.to_string()).is_none(), "{value}");
    }

    for reason in ["future_reason", "provider_error"] {
        let mut value = permitted.clone();
        value["outcome"]["reason"] = Value::String(reason.into());
        assert!(discard::decode(&value.to_string()).is_none(), "{value}");
    }
}

#[test]
fn retry_suppressed_schema_is_closed_and_exposes_no_memory_identity() {
    let schema =
        serde_json::to_value(schemars::schema_for!(discard::CompactionOutcomeFact)).unwrap();
    let text = schema.to_string();
    assert!(text.contains("retry_suppressed"));
    assert!(text.contains("preparation"));
    for private in [
        "failure_identity",
        "provider_turn_key",
        "owner_instance_id",
        "run_id",
        "turn_id",
    ] {
        assert!(!text.contains(private), "schema leaks {private}");
    }
}
