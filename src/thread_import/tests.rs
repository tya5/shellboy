use super::*;

fn archive(records: &str) -> String {
    let preceding = format!(
        "{{\"type\":\"header\",\"format\":\"shellboy-thread-export\",\"version\":1,\"core_protocol\":11,\"thread\":{{\"thread_id\":\"source\"}}}}\n{records}"
    );
    let digest = Sha256::digest(preceding.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "{preceding}{{\"type\":\"trailer\",\"lines\":{},\"sha256\":\"{digest}\"}}\n",
        preceding.lines().count()
    )
}

#[test]
fn thread_import_checks_exact_lf_and_digest_before_metadata() {
    let valid = archive("");
    let parsed = validate(&valid).unwrap();
    assert_eq!(parsed.source_thread, "source");
    assert!(parsed.records.is_empty());
    assert!(
        validate(&valid.replacen("source", "other!", 1))
            .err()
            .unwrap()
            .message
            .contains("SHA-256")
    );
    assert!(validate(valid.trim_end()).is_err());
    assert_eq!(
        validate(&"x".repeat(MAX_FRAME_BYTES + 1))
            .err()
            .unwrap()
            .code,
        ErrorCode::InputLimit
    );
}

#[test]
fn thread_import_refuses_unresolved_internal_references() {
    let archive = archive(concat!(
        r#"{"type":"tombstone","role":"current","node":{"id":"first","order":1,"current_ordinal":0,"provenance":{"kind":"user","created_at":"now","source_revision":0}},"relations":{"previous":[{"id":"absent","scope":"internal"}],"next":[],"replacement_groups":[],"replaced_by":[]},"reason":"pruned"}"#,
        "\n"
    ));
    assert!(validate(&archive).is_err());
}
