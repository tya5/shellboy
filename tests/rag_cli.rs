//! Native standalone CLI: synthetic materials only, no server or credentials.
use serde_json::Value;
use std::path::Path;

fn call(root: &Path, args: &[&str], success: bool) -> Value {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .current_dir(root)
        .env_clear()
        .arg("--data-dir")
        .arg(root.join("data"))
        .arg("--config")
        .arg(root.join("not-configured.toml"))
        .arg("rag")
        .arg("--db")
        .arg(root.join("knowledge.db"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], success, "{value}");
    value
}
fn setup(root: &Path) {
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(
        root.join("materials/notes.md"),
        "初期の復旧手順 SHELLBOY_KNOWLEDGE_OLD 100%_literal",
    )
    .unwrap();
    call(root, &["init"], true);
    call(
        root,
        &["source", "add", "materials", "--name", "notes"],
        true,
    );
    call(root, &["index", "--source", "notes"], true);
}
fn search(root: &Path, query: &str) -> Value {
    call(
        root,
        &["search", "--scope", "source:notes", "--query", query],
        true,
    )
}

#[test]
fn docs_scope_does_not_create_a_database_and_unknown_scope_is_not_empty_success() {
    let root = tempfile::tempdir().unwrap();
    let docs = call(
        root.path(),
        &["search", "--scope", "docs", "--query", "復旧"],
        true,
    );
    assert!(!docs["result"]["items"].as_array().unwrap().is_empty());
    assert!(!root.path().join("knowledge.db").exists());
    assert!(!root.path().join("data").exists());
    let partial = call(
        root.path(),
        &[
            "search",
            "--scope",
            "docs",
            "--scope",
            "source:missing",
            "--query",
            "復旧",
        ],
        false,
    );
    assert!(!partial["result"]["items"].as_array().unwrap().is_empty());
    assert_eq!(
        partial["result"]["scope_errors"].as_array().unwrap().len(),
        1
    );
    call(root.path(), &["status"], false);
    assert!(!root.path().join("knowledge.db").exists());
}

#[test]
fn literal_japanese_search_diff_sync_saved_revision_and_unregister_preserve_originals() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    setup(root);
    let result = search(root, "復旧");
    let reference = result["result"]["items"][0]["reference"].as_str().unwrap();
    assert_eq!(
        search(root, "100%_literal")["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        search(root, "100anythingliteral")["result"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        call(root, &["index", "--source", "notes"], true)["result"]["changed"],
        0
    );
    std::fs::write(
        root.join("materials/notes.md"),
        "変更した再開手順 SHELLBOY_KNOWLEDGE_NEW",
    )
    .unwrap();
    call(root, &["index", "--source", "notes", "--dry-run"], true);
    assert!(
        !search(root, "SHELLBOY_KNOWLEDGE_OLD")["result"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    call(root, &["index", "--source", "notes"], true);
    assert!(
        search(root, "SHELLBOY_KNOWLEDGE_OLD")["result"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let saved = call(root, &["get", "--ref", reference], true);
    assert!(
        saved["result"]["text"]
            .as_str()
            .unwrap()
            .contains("SHELLBOY_KNOWLEDGE_OLD")
    );
    std::fs::rename(
        root.join("materials/notes.md"),
        root.join("materials/renamed.md"),
    )
    .unwrap();
    let renamed = call(root, &["index", "--source", "notes"], true);
    assert_eq!(renamed["result"]["removed"], 1);
    assert_eq!(
        search(root, "SHELLBOY_KNOWLEDGE_NEW")["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    std::fs::remove_file(root.join("materials/renamed.md")).unwrap();
    let deleted = call(root, &["index", "--source", "notes"], true);
    assert_eq!(deleted["result"]["removed"], 1);
    assert!(
        search(root, "SHELLBOY_KNOWLEDGE_NEW")["result"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    call(root, &["get", "--ref", reference], true);
    std::fs::write(
        root.join("materials/notes.md"),
        "変更した再開手順 SHELLBOY_KNOWLEDGE_NEW",
    )
    .unwrap();
    call(root, &["index", "--source", "notes"], true);
    call(root, &["source", "remove", "--source", "notes"], true);
    assert_eq!(
        std::fs::read_to_string(root.join("materials/notes.md")).unwrap(),
        "変更した再開手順 SHELLBOY_KNOWLEDGE_NEW"
    );
    let removed = call(root, &["get", "--ref", reference], false);
    assert_eq!(removed["error"]["code"], "revision_unavailable");
    assert!(!root.join("data").exists());
}

#[test]
fn missing_source_root_retains_the_published_revision_and_reports_failure() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    setup(root);
    std::fs::rename(root.join("materials"), root.join("temporarily-offline")).unwrap();
    let result = call(root, &["index", "--source", "notes"], false);
    assert!(result["result"]["partial_failure"] == true || result["error"]["code"].is_string());
    assert!(
        !search(root, "SHELLBOY_KNOWLEDGE_OLD")["result"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn external_explanations_are_idempotent_and_not_overwritten_by_file_sync() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    setup(root);
    std::fs::write(
        root.join("explanation.md"),
        "外部生成済み SHELLBOY_DERIVED_V1",
    )
    .unwrap();
    let words = [
        "ingest",
        "--id",
        "explanation",
        "--source-file",
        "materials/notes.md",
        "--kind",
        "llm-explanation",
        "--text-file",
        "explanation.md",
    ];
    let first = call(root, &words, true);
    let again = call(root, &words, true);
    assert_eq!(first["result"]["revision"], again["result"]["revision"]);
    call(root, &["index"], true);
    let found = call(
        root,
        &[
            "search",
            "--scope",
            "source:derived",
            "--query",
            "SHELLBOY_DERIVED_V1",
        ],
        true,
    );
    let reference = found["result"]["items"][0]["reference"].as_str().unwrap();
    let mut cursor: Option<String> = None;
    let mut text = String::new();
    loop {
        let mut words = vec!["get", "--ref", reference, "--limit-bytes", "7"];
        if let Some(value) = &cursor {
            words.extend(["--cursor", value]);
        }
        let page = call(root, &words, true);
        text.push_str(page["result"]["text"].as_str().unwrap());
        cursor = page["result"]["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(text, "外部生成済み SHELLBOY_DERIVED_V1");
}

#[test]
fn semantic_mode_requires_configuration_and_does_not_implicitly_fall_back() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    setup(root);
    let value = call(
        root,
        &[
            "search",
            "--scope",
            "source:notes",
            "--query",
            "復旧",
            "--mode",
            "semantic",
        ],
        false,
    );
    assert!(
        value["result"]["items"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
}

#[test]
fn keyword_match_crosses_chunk_boundaries_and_reference_cursors_cannot_be_swapped() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    setup(root);
    let text = format!(
        "{}SHELLBOY_CROSS_BOUNDARY_V1{}",
        "界".repeat(1995),
        "終".repeat(40)
    );
    std::fs::write(root.join("materials/notes.md"), &text).unwrap();
    call(root, &["index"], true);
    let found = search(root, "SHELLBOY_CROSS_BOUNDARY_V1");
    let reference = found["result"]["items"][0]["reference"].as_str().unwrap();
    let page = call(
        root,
        &["get", "--ref", reference, "--limit-bytes", "7"],
        true,
    );
    let cursor = page["result"]["next_cursor"].as_str().unwrap();
    std::fs::write(
        root.join("materials/other.md"),
        "別の復旧文書 SHELLBOY_OTHER_V1",
    )
    .unwrap();
    call(root, &["index"], true);
    let other = search(root, "SHELLBOY_OTHER_V1");
    let other_ref = other["result"]["items"][0]["reference"].as_str().unwrap();
    let failure = call(
        root,
        &["get", "--ref", other_ref, "--cursor", cursor],
        false,
    );
    assert_eq!(failure["error"]["code"], "cursor_expired");
    assert_eq!(found["result"]["items"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_reader_works_with_live_and_stopped_owner_and_expires_replaced_db() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let config = root.join("owner.toml");
    std::fs::write(&config, "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
    let owner = shellboy::app::DataOwner::start(Some(config), Some(root.join("data")), root.into())
        .await
        .unwrap();
    owner.db.call(|db| {
        let tx = db.transaction()?;
        tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('thread','/','/AGENTS.md','fixture','fixture','now')", [])?;
        shellboy::history::append_node(&tx, "thread", "history-source", None, "user", &serde_json::json!({"role":"user","content":[{"type":"input_text","text":"履歴復旧 SHELLBOY_READ_ONLY_HISTORY_V1"}]}), "now")?;
        tx.commit()?; Ok(())
    }).await.unwrap();
    let args = [
        "search",
        "--scope",
        "history",
        "--query",
        "SHELLBOY_READ_ONLY_HISTORY_V1",
    ];
    let live = call(root, &args, true);
    let reference = live["result"]["items"][0]["reference"].as_str().unwrap();
    assert_eq!(
        live["result"]["items"][0]["provenance"]["current_thread_paths"][0],
        "thread"
    );
    call(root, &["get", "--ref", reference], true);
    owner.shutdown().await.unwrap();
    let stopped = call(root, &args, true);
    assert_eq!(
        live["result"]["items"][0]["reference"],
        stopped["result"]["items"][0]["reference"]
    );
    let db = root.join("data/shellboy.db");
    std::fs::copy(&db, root.join("replacement.db")).unwrap();
    std::fs::rename(root.join("replacement.db"), &db).unwrap();
    let replaced = call(root, &["get", "--ref", reference], false);
    assert_eq!(replaced["error"]["code"], "revision_unavailable");
    assert!(!root.join("knowledge.db").exists());
}

#[test]
fn bundled_guide_index_has_an_explicit_cost_preview_without_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    call(root, &["init"], true);
    let preview = call(
        root,
        &[
            "index",
            "--source",
            "docs",
            "--mode",
            "semantic",
            "--dry-run",
        ],
        true,
    );
    assert!(preview["result"]["documents"].as_u64().unwrap() > 0);
    assert!(preview["result"]["text_bytes"].as_u64().unwrap() > 0);
    assert_eq!(preview["result"]["embedding_may_charge"], true);
    assert_eq!(call(root, &["status"], true)["result"]["documents"], 0);
    assert_eq!(
        call(root, &["search", "--scope", "", "--query", "復旧"], false)["result"]["scope_errors"]
            [0]["code"],
        "scope_invalid"
    );
    call(
        root,
        &["search", "--scope", "docs", "--query", "復旧"],
        true,
    );
    assert_eq!(
        call(
            root,
            &[
                "index",
                "--source",
                "docs",
                "--extractor",
                "missing-extractor"
            ],
            false
        )["error"]["code"],
        "invalid_argument"
    );
    assert!(!root.join("data/shellboy.db").exists());
}
