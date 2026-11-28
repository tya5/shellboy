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
    let command = hint.split_once("shellboy docs ").unwrap().1;
    let words = ["docs"]
        .into_iter()
        .chain(command.split_ascii_whitespace())
        .collect::<Vec<_>>();
    std::fs::write(root.path().join("missing.toml"), "[broken\n").unwrap();
    let (success, found) = call(root.path(), &words);
    assert!(success && found["ok"] == true, "{found}");
    assert_eq!(found["result"]["id"], "shellboy/shell");
    assert!(
        found["result"]["text"]
            .as_str()
            .unwrap()
            .contains("shell background --execution")
    );
    std::fs::remove_file(root.path().join("missing.toml")).unwrap();
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
                "explicit-inspection-of-thread-completion",
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
            "last_end_message",
            "last_input",
            "interrupted wait",
        ] {
            assert!(text.contains(required), "{text}");
        }
        assert!(!root.path().join("uncreated").exists());
    })
    .await
    .unwrap();
}

#[test]
fn resource_usage_guide_is_bundled_searchable_and_offline() {
    let root = tempfile::tempdir().unwrap();
    let mut cursor: Option<String> = None;
    let mut text = String::new();
    for page_number in 0..100 {
        let mut args = vec!["docs", "get", "--id", "shellboy/resource-usage"];
        if let Some(value) = &cursor {
            args.extend(["--cursor", value]);
        }
        let (success, page) = call(root.path(), &args);
        assert!(success && page["ok"] == true, "{page}");
        text.push_str(page["result"]["text"].as_str().unwrap());
        match page["result"].get("next_cursor") {
            None => break,
            Some(Value::String(value)) => cursor = Some(value.clone()),
            Some(other) => panic!("terminal cursor must be omitted: {other}"),
        }
        assert!(page_number < 99, "document pagination did not terminate");
    }
    for required in [
        "workflow run 41307629104",
        "Workflow run 41405053536",
        "TUI-hosted workload observations (#151 objection 7)",
        "Run 41526337673",
        "1e7ddd1e17fc1d2ebfefa19388f3f4275b6859ab",
        "| 1 | 25,493 / 25,521 / 26,407 | 20,730 / 20,851 / 20,873 |",
        "| 5 | 27,864 / 28,114 / 29,824 | 24,464 / 25,046 / 25,170 |",
        "| 10 | 29,462 / 32,720 / 32,822 | 26,412 / 27,132 / 27,268 |",
        "| 20 | 34,071 / 34,223 / 34,627 | 28,564 / 28,576 / 28,829 |",
        "the difference is not a TUI increment",
        "compare job and overall run failed",
        "artifact 13181231326",
        "run 41553439861",
        "2669c1ba59eadce04a1b800bc12e56c34606ae4c",
        "passed compare",
        "That is not a\nrerun of the TUI-hosted experiment",
        "No new\nbenchmark was performed",
        "spawn-to-echo, not keystroke-to-echo latency",
        "5a2e8474a52928eaafd35d64c3eb29dfd6c34626",
        "Stage 1: Shellboy alone",
        "Stage 2: same-run agent comparison",
        "PSS in KiB",
        "2026-11-01",
        "within the same comparison run and artifact",
        "Approvals and sandboxes are disabled",
        "not an internal ready timestamp",
        "not interchangeable",
        "JCODE_MEMORY_ENABLED=0",
        "Embeddings use a configured external API",
        "scripts/bench/compare.py",
    ] {
        assert!(text.contains(required), "resource guide missing {required}");
    }
    let (success, search) = call(
        root.path(),
        &[
            "docs",
            "search",
            "--parent",
            "shellboy/reference",
            "--query",
            "JCODE_MEMORY_ENABLED",
        ],
    );
    assert!(success && search["ok"] == true, "{search}");
    assert!(
        search["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["id"] == "shellboy/resource-usage" })
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn startup_stage_status_keeps_unavailable_exit_and_prints_last_stage() {
    let root = tempfile::tempdir().unwrap();
    let status = || {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(root.path())
            .arg("status")
            .env_remove("SHELLBOY_ENDPOINT")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["error"]["code"], "app_not_running");
        envelope["error"]["details"]["last_startup_stage"].clone()
    };
    assert!(status().is_null());
    std::fs::write(
        root.path().join("startup-stage.json"),
        b"\"database_migration\"",
    )
    .unwrap();
    assert_eq!(status(), "database_migration");
    std::fs::write(root.path().join("startup-stage.json"), b"\"unknown\"").unwrap();
    assert!(status().is_null());
}

#[test]
fn bundled_recovery_does_not_parse_broken_config() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("missing.toml"), "[broken\n").unwrap();
    for args in [
        vec!["docs", "list", "--source", "shellboy"],
        vec![
            "docs",
            "search",
            "--source",
            "shellboy",
            "--query",
            "configuration",
        ],
        vec!["docs", "get", "--id", "shellboy/configuration"],
    ] {
        let (success, value) = call(root.path(), &args);
        assert!(success, "{value}");
    }
    let (success, value) = call(root.path(), &["docs", "list"]);
    assert!(!success);
    assert_eq!(value["error"]["code"], "config_invalid");
}

#[test]
fn configured_search_resumes_an_empty_work_page() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let folder = root.join("guides");
    std::fs::create_dir(&folder).unwrap();
    for n in 0..225 {
        std::fs::write(folder.join(format!("{n:03}.md")), "# Ordinary guide\n").unwrap();
    }
    std::fs::write(
        folder.join("last.md"),
        "# Last guide\nneedle-past-scan-boundary\n",
    )
    .unwrap();
    let (ok, registered) = call(
        root,
        &[
            "docs", "source", "add", "--name", "local", "--path", "guides",
        ],
    );
    assert!(ok, "{registered}");
    let args = [
        "docs",
        "search",
        "--parent",
        "local",
        "--query",
        "needle-past-scan-boundary",
    ];
    let (ok, first) = call(root, &args);
    assert!(ok, "{first}");
    let mut page = first;
    let mut seen = std::collections::HashSet::new();
    let mut found = false;
    loop {
        found |= page["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "local/last.md");
        let Some(cursor) = page["result"]["next_cursor"].as_str() else {
            break;
        };
        assert!(seen.insert(cursor.to_owned()), "search cursor repeated");
        let mut args = args.to_vec();
        args.extend(["--cursor", cursor]);
        let (ok, next) = call(root, &args);
        assert!(ok, "{next}");
        page = next;
    }
    assert!(found);
}

#[test]
fn configured_list_resumes_after_examining_non_guides_in_a_wide_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let folder = root.join("guides");
    std::fs::create_dir(&folder).unwrap();
    for n in (0..205).rev() {
        std::fs::write(folder.join(format!("{n:03}.txt")), "not a guide").unwrap();
    }
    std::fs::write(folder.join("last.md"), "# Last guide\n").unwrap();
    let (ok, registered) = call(
        root,
        &[
            "docs", "source", "add", "--name", "local", "--path", "guides",
        ],
    );
    assert!(ok, "{registered}");
    std::fs::write(folder.join("z-last.md"), "# Final guide\n").unwrap();
    let args = ["docs", "list", "--parent", "local", "--limit", "1"];
    let (ok, mut page) = call(root, &args);
    assert!(ok, "{page}");
    let cursor = page["result"]["next_cursor"].as_str().unwrap().to_owned();
    let mut ids = Vec::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        ids.extend(
            page["result"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["id"].as_str().unwrap().to_owned()),
        );
        let Some(next) = page["result"]["next_cursor"].as_str() else {
            break;
        };
        assert!(seen.insert(next.to_owned()), "list cursor repeated");
        let mut args = args.to_vec();
        args.extend(["--cursor", next]);
        let (ok, next) = call(root, &args);
        assert!(ok, "{next}");
        page = next;
    }
    assert_eq!(ids, ["local/last.md", "local/z-last.md"]);
    std::fs::write(folder.join("new.md"), "# New guide\n").unwrap();
    let (ok, stale) = call(
        root,
        &["docs", "list", "--parent", "local", "--cursor", &cursor],
    );
    assert!(!ok, "{stale}");
    assert_eq!(stale["error"]["code"], "cursor_expired");
}

#[test]
fn configured_docs_project_metadata_and_preview_from_the_folder() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let folder = root.join("guides");
    std::fs::create_dir(&folder).unwrap();
    let title = "Guide ".repeat(200) + "heading marker";
    let summary = "Reference ".repeat(120) + "summary marker";
    let text = format!("# {title}\n\n{summary}\n");
    std::fs::write(folder.join("guide.md"), &text).unwrap();
    let (ok, registered) = call(
        root,
        &[
            "docs", "source", "add", "--name", "local", "--path", "guides",
        ],
    );
    assert!(ok, "{registered}");
    let (ok, list) = call(root, &["docs", "list", "--parent", "local"]);
    assert!(ok, "{list}");
    assert_eq!(list["result"]["items"][0]["title"], title);
    assert_eq!(list["result"]["items"][0]["summary"], summary);
    // Include entries the projection ignores; the walker still has to pass them.
    for i in 0..225 {
        std::fs::write(folder.join(format!("{i:03}.txt")), "not a guide").unwrap();
    }
    let (ok, preview) = call(root, &["docs", "index", "--source", "local", "--dry-run"]);
    assert!(ok, "{preview}");
    let row = &preview["result"]["sources"][0];
    assert_eq!(row["complete"], true);
    assert_eq!(row["files"], 1);
    assert_eq!(row["bytes"], text.len());
    assert!(!root.join("uncreated").exists());
}
