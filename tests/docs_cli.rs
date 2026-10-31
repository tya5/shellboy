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
        vec!["docs", "search", "--query", "background"],
    ] {
        let (success, value) = call(root.path(), &args);
        assert!(success && value["ok"] == true, "{value}");
        if args.get(1) == Some(&"search") {
            assert!(!value["result"]["items"].as_array().unwrap().is_empty());
        }
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

#[test]
fn protocol_recovery_hint_finds_real_offline_shell_examples() {
    let root = tempfile::tempdir().unwrap();
    // Owner RPC errors use Response::failure; standalone docs errors have a
    // separate envelope and are not expected to contain this recovery hint.
    let response = shellboy::protocol::Response::<Value>::failure(
        "fixture:invalid-argument",
        shellboy::protocol::ApiError::new(
            shellboy::protocol::ErrorCode::InvalidArgument,
            "fixture",
        ),
    );
    let value = serde_json::to_value(response).unwrap();
    let hint = value["error"]["hint"].as_str().unwrap();
    let query = hint
        .split("--query '")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    let (success, found) = call(root.path(), &["docs", "search", "--query", query]);
    assert!(success && found["ok"] == true, "{found}");
    assert!(
        found["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "shellboy/shell"),
        "Recovery hint must find executable shell examples: {found}"
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn agent_semantic_search_guidance_is_bundled_and_examples_match_the_cli() {
    crate::test_watchdog::watchdog(
        "tests/docs_cli.rs: agent_semantic_search_guidance_is_bundled_and_examples_match_the_cli",
        async {
            use clap::Parser;
            let root = tempfile::tempdir().unwrap();
            let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
                .current_dir(root.path())
                .env_clear()
                .kill_on_drop(true)
                .args([
                    "--config",
                    root.path().join("missing.toml").to_str().unwrap(),
                    "--data-dir",
                    root.path().join("uncreated").to_str().unwrap(),
                    "docs",
                    "get",
                    "--id",
                    "shellboy/agent-guide",
                    "--section",
                    "when-semantic-history-search-helps",
                ])
                .output()
                .await
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
            assert!(output.stderr.is_empty());
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            let text = value["result"]["text"].as_str().unwrap();
            for required in [
                "omitted by automatic reduction",
                "hundreds of nodes",
                "another conversation recovered",
                "Windows CI timeouts",
                "PTY test failures",
                "GitHub API 502s",
                "truncated model requests",
                "release",
                "installer validation",
                "merge-conflict",
                "reasons for an earlier decision",
                "one query-embedding API request",
                "no search is needed",
                "not new user authorization",
                "verify today's",
                "not just your selected path",
                "rag get --ref REFERENCE",
            ] {
                assert!(text.contains(required), "missing {required}: {text}");
            }
            assert_eq!(
                text.lines().filter(|line| line.starts_with("- ")).count(),
                5
            );
            let mut examples = 0;
            for line in text.lines().filter(|line| line.starts_with("shellboy ")) {
                let (prefix, quoted) = line.split_once(" --query '").unwrap();
                let (query, suffix) = quoted.rsplit_once("' --limit ").unwrap();
                let substituted = prefix.replace("\"$SHELLBOY_THREAD_ID\"", "THIS_THREAD");
                let mut args = substituted.split_whitespace().collect::<Vec<_>>();
                args.extend(["--query", query, "--limit", suffix]);
                shellboy::cli::Cli::try_parse_from(args).unwrap();
                examples += 1;
            }
            assert_eq!(examples, 3);
            shellboy::cli::Cli::try_parse_from(["shellboy", "rag", "get", "--ref", "REFERENCE"])
                .unwrap();
            assert!(!root.path().join("uncreated").exists());
        },
    )
    .await
    .expect("bundled guide watchdog");
}

#[tokio::test]
async fn bundled_agent_guide_explains_thread_wait_continuation_without_an_owner() {
    crate::test_watchdog::watchdog("bundled Thread wait guide", async {
        let root = tempfile::tempdir().unwrap();
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .env_clear()
            .current_dir(root.path())
            .kill_on_drop(true)
            .arg("--config")
            .arg(root.path().join("missing.toml"))
            .arg("--data-dir")
            .arg(root.path().join("uncreated"))
            .args([
                "docs",
                "get",
                "--id",
                "shellboy/agent-guide",
                "--section",
                "wait-for-any-child-thread",
            ])
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
        let text = reply["result"]["text"].as_str().unwrap();
        for required in [
            "thread_settled",
            "--after",
            "result.snapshot.changes",
            "result.cursor",
            "last_answer",
            "last_input",
            "owner restart",
        ] {
            assert!(text.contains(required), "{text}");
        }
        assert!(!root.path().join("uncreated").exists());
    })
    .await
    .unwrap();
}
