//! The installed executable carries its guide catalogue and needs no owner/config.
use serde_json::Value;

fn call(root: &std::path::Path, words: &[&str]) -> (bool, Value) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .current_dir(root)
        .env_clear()
        .arg("--config")
        .arg(root.join("missing.toml"))
        .arg("--data-dir")
        .arg(root.join("uncreated"))
        .args(words)
        .output()
        .unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        output.status.success(),
        serde_json::from_slice(&output.stdout).unwrap(),
    )
}

#[test]
fn binary_guides_are_offline_and_independent_of_configuration_and_cwd() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["docs", "list"],
        vec!["docs", "get", "--id", "shellboy/shell"],
        vec!["docs", "search", "--query", "背景"],
    ] {
        let (success, value) = call(root.path(), &args);
        assert!(success && value["ok"] == true, "{value}");
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn local_commands_reject_owner_operation_ids_and_unknown_guides_as_json() {
    let root = tempfile::tempdir().unwrap();
    let (success, value) = call(root.path(), &["--operation-id", "x:y", "docs", "list"]);
    assert!(!success);
    assert_eq!(value["error"]["code"], "invalid_argument");
    let (success, value) = call(root.path(), &["docs", "get", "--id", "missing"]);
    assert!(!success && value["ok"] == false);
    assert!(!root.path().join("uncreated").exists());
}

#[test]
fn document_pages_omit_terminal_cursor_and_follow_real_continuations_offline() {
    let root = tempfile::tempdir().unwrap();
    for words in [
        vec!["docs", "list", "--parent", "shellboy", "--limit", "2"],
        vec![
            "docs",
            "get",
            "--id",
            "shellboy/shell",
            "--limit-bytes",
            "512",
        ],
        vec!["docs", "search", "--query", "shell", "--limit", "2"],
    ] {
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        let mut ids = std::collections::BTreeSet::new();
        loop {
            let mut args = words.clone();
            if let Some(value) = &cursor {
                args.extend(["--cursor", value]);
            }
            let (success, page) = call(root.path(), &args);
            assert!(success && page["ok"] == true, "{page}");
            let result = page["result"].as_object().unwrap();
            if let Some(items) = result.get("items").and_then(Value::as_array) {
                for item in items {
                    assert!(ids.insert(item["id"].as_str().unwrap().to_owned()));
                }
            }
            pages += 1;
            assert!(pages <= 100, "document pagination did not terminate");
            match result.get("next_cursor") {
                None => break,
                Some(Value::String(value)) => cursor = Some(value.clone()),
                Some(other) => panic!("terminal cursor must be omitted: {other}"),
            }
        }
        assert!(pages > 1, "fixture must exercise a real continuation");
    }
    let (success, empty) = call(
        root.path(),
        &[
            "docs",
            "search",
            "--query",
            "SHELLBOY_NO_SUCH_GUIDE_MATCH_87654321",
        ],
    );
    assert!(success);
    assert!(empty["result"]["items"].as_array().unwrap().is_empty());
    assert!(empty["result"].get("next_cursor").is_none());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
