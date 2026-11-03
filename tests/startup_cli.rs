use serde_json::Value;
use std::{fs, process::Command};

fn stage(at: &str, name: &str, elapsed: u64, version: Option<&str>) -> String {
    let mut value = serde_json::json!({
        "at": at,
        "event": "startup_stage",
        "ids": {"thread_id": "never-print-this"},
        "startup_stage": {"stage": name, "elapsed_ms": elapsed, "migration_version": 25},
    });
    if let Some(version) = version {
        value["startup_stage"]["app_version"] = serde_json::json!(version);
    }
    value.to_string()
}

#[test]
fn startup_cli_reads_synthetic_rotated_logs_without_owner_or_leaking_other_fields() {
    let root = tempfile::tempdir().unwrap();
    let logs = root.path().join("logs");
    fs::create_dir_all(&logs).unwrap();
    let config = root.path().join("config.toml");
    fs::write(&config, "").unwrap();
    fs::write(
        logs.join("shellboy.log.1"),
        format!(
            "{}\n{{broken\n",
            stage("2026-01-01T00:00:00Z", "configuration", 4, Some("0.7.1"))
        ),
    )
    .unwrap();
    fs::write(
        logs.join("shellboy.log"),
        format!(
            "{}\n{}\n{}\n",
            stage(
                "2026-01-01T00:00:01Z",
                "database_migration",
                9,
                Some("0.7.1")
            ),
            stage("2026-01-01T00:00:02Z", "endpoint", 3, Some("0.7.1")),
            stage("2026-02-01T00:00:00Z", "configuration", 2, Some("0.7.2")),
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .arg("--data-dir")
        .arg(root.path())
        .arg("--config")
        .arg(&config)
        .arg("startup")
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let launches = value["result"]["launches"].as_array().unwrap();
    assert_eq!(launches.len(), 2);
    assert_eq!(launches[0]["migration_version"], 25);
    assert_eq!(launches[0]["version_increased"], Value::Null);
    assert_eq!(launches[1]["version_increased"], true);
    assert_eq!(launches[1]["incomplete"], true);
    assert_eq!(value["result"]["invalid_lines_seen"], true);
    assert_eq!(value["result"]["records_may_be_missing"], true);
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(!output.contains("never-print-this"));
    assert!(!output.contains("broken"));
    assert!(!root.path().join("shellboy.db").exists());

    let invalid = Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .arg("--data-dir")
        .arg(root.path())
        .arg("--config")
        .arg(&config)
        .args(["startup", "--last", "21"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    let error: Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert_eq!(error["error"]["code"], "invalid_argument");
}
