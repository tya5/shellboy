//! #21: synthetic, offline readiness diagnostics and explicit hybrid degradation.
use serde_json::{Value, json};
use shellboy::rag::{IndexMode, RagCommand, RagOptions, SearchMode, SourceCommand, execute};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU16, AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn call(root: &Path, command: RagCommand) -> Value {
    execute(
        RagOptions {
            db: Some(root.join("knowledge.db")),
            command,
        },
        Some(root.join("config.toml")),
        Some(root.join("data")),
        root,
    )
    .await
}
fn config(root: &Path, address: &str) {
    std::fs::write(root.join("config.toml"), format!(
        "[connections.fixture]\nbase_url='http://{address}/v1'\n[embedding]\nconnection='fixture'\nmodel='synthetic'\n")).unwrap();
}
async fn source(root: &Path, name: &str) {
    std::fs::create_dir(root.join(name)).unwrap();
    std::fs::write(
        root.join(name).join("note.md"),
        format!("readiness marker {name}"),
    )
    .unwrap();
    let result = call(
        root,
        RagCommand::Source {
            command: SourceCommand::Add {
                path: name.into(),
                name: name.into(),
                exclude: vec![],
                clear_excludes: false,
            },
        },
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(index(root, name, IndexMode::Keyword).await["ok"], true);
}
async fn index(root: &Path, name: &str, mode: IndexMode) -> Value {
    call(
        root,
        RagCommand::Index {
            source: Some(name.into()),
            dry_run: false,
            mode,
            extractor: None,
            adapter: None,
        },
    )
    .await
}
async fn search(root: &Path, scopes: &[&str], mode: SearchMode) -> Value {
    call(
        root,
        RagCommand::Search {
            query: "readiness".into(),
            scope: scopes.iter().map(|s| (*s).into()).collect(),
            mode,
            limit: 100,
        },
    )
    .await
}

#[tokio::test]
async fn unprepared_materials_keep_keyword_ranks_and_group_preparation_warnings() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    config(root, "127.0.0.1:9");
    assert_eq!(call(root, RagCommand::Init).await["ok"], true);
    source(root, "first").await;
    source(root, "second").await;
    let scopes = ["source:first", "source:second"];
    let keyword = search(root, &scopes, SearchMode::Keyword).await;
    let hybrid = search(root, &scopes, SearchMode::Hybrid).await;
    assert_eq!(hybrid["ok"], true, "{hybrid}");
    assert!(!hybrid.to_string().contains("rag_semantic_"));
    assert_eq!(hybrid["result"]["degraded"], true);
    assert_eq!(hybrid["result"]["items"], keyword["result"]["items"]);
    let warnings = hybrid["result"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{hybrid}");
    assert_eq!(warnings[0]["scopes"], json!(scopes));
    for status in hybrid["result"]["scope_status"].as_array().unwrap() {
        assert_eq!(status["keyword"], "ok");
        assert_eq!(status["semantic"], "not_ready");
        assert_eq!(status["reason"], "profile_not_indexed");
        assert_eq!(status["missing_documents"], 1);
        assert_eq!(status["missing_chunks"], 1);
        let steps = status["next"]["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        let command = steps[1]["command"].as_array().unwrap();
        let selected = command.iter().position(|v| v == "--source").unwrap() + 1;
        assert_eq!(command[selected], status["source"]);
        assert!(command.contains(&json!(root.join("knowledge.db").to_string_lossy())));
    }
    assert!(
        hybrid["result"]["scope_errors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["warning"] == true)
    );
    let semantic = search(root, &scopes, SearchMode::Semantic).await;
    assert_eq!(semantic["ok"], false);
    assert_eq!(semantic["error"]["code"], "partial_failure");
    assert_eq!(semantic["result"]["degraded"], false);
}

#[tokio::test]
async fn missing_docs_database_is_explicit_degradation_without_creating_storage() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    config(root, "127.0.0.1:9");
    let hybrid = search(root, &["docs"], SearchMode::Hybrid).await;
    assert_eq!(hybrid["ok"], true, "{hybrid}");
    assert_eq!(hybrid["result"]["degraded"], true);
    let status = &hybrid["result"]["scope_status"][0];
    assert_eq!(status["reason"], "store_not_initialized");
    assert!(status["missing_documents"].as_i64().unwrap() > 0);
    assert!(status["missing_chunks"].is_null());
    assert_eq!(
        status["next"]["steps"][0]["command"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        "init"
    );
    assert_eq!(
        search(root, &["docs"], SearchMode::Semantic).await["ok"],
        false
    );
    assert!(!root.join("knowledge.db").exists());
    assert!(!root.join("data").exists());
    std::fs::write(root.join("config.toml"), "invalid = [").unwrap();
    let invalid = search(root, &["docs"], SearchMode::Hybrid).await;
    assert_eq!(invalid["ok"], false);
    assert_eq!(
        invalid["result"]["scope_errors"][0]["code"],
        "config_invalid"
    );
    assert_eq!(invalid["result"]["degraded"], false);
}

async fn proxy(
    root: &Path,
) -> (
    Arc<AtomicU16>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    config(root, &listener.local_addr().unwrap().to_string());
    let code = Arc::new(AtomicU16::new(200));
    let requests = Arc::new(AtomicUsize::new(0));
    let (status, count) = (code.clone(), requests.clone());
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let (header_end, length) = loop {
                let n = stream.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                assert!(bytes.len() <= 1024 * 1024);
                if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while bytes.len() < header_end + length {
                let n = stream.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                assert!(bytes.len() <= 1024 * 1024);
            }
            count.fetch_add(1, Ordering::SeqCst);
            let input: Value =
                serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
            let code = status.load(Ordering::SeqCst);
            let body = if code == 200 {
                json!({"data":input["input"].as_array().unwrap().iter().enumerate().map(|(i,_)|
                    json!({"index":i,"embedding":[0.1,0.2]})).collect::<Vec<_>>(),"usage":{"prompt_tokens":2,"total_tokens":2}})
            } else { json!({"error":{"message":"synthetic failure"}}) }.to_string();
            stream.write_all(format!("HTTP/1.1 {code} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    (code, requests, server)
}

#[tokio::test]
async fn ready_hybrid_stays_hybrid_and_partial_coverage_reports_exact_missing_counts() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (_code, requests, server) = proxy(root).await;
    call(root, RagCommand::Init).await;
    source(root, "notes").await;
    assert_eq!(index(root, "notes", IndexMode::Semantic).await["ok"], true);
    let ready = search(root, &["source:notes"], SearchMode::Hybrid).await;
    assert_eq!(ready["ok"], true, "{ready}");
    assert_eq!(ready["result"]["degraded"], false);
    assert_eq!(ready["result"]["scope_status"][0]["semantic"], "ok");
    assert_eq!(ready["result"]["items"][0]["method"], "hybrid");
    let before = requests.load(Ordering::SeqCst);
    let db = rusqlite::Connection::open(root.join("knowledge.db")).unwrap();
    db.execute("DELETE FROM embeddings", []).unwrap();
    let incomplete = search(root, &["source:notes"], SearchMode::Hybrid).await;
    assert_eq!(incomplete["ok"], true, "{incomplete}");
    assert_eq!(
        incomplete["result"]["scope_status"][0]["reason"],
        "embeddings_incomplete"
    );
    assert_eq!(
        incomplete["result"]["scope_status"][0]["missing_documents"],
        1
    );
    assert_eq!(incomplete["result"]["scope_status"][0]["missing_chunks"], 1);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        before,
        "readiness does not request embedding"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn provider_failure_and_incompatible_vector_identity_are_not_successful_fallbacks() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (code, _, server) = proxy(root).await;
    call(root, RagCommand::Init).await;
    source(root, "notes").await;
    assert_eq!(index(root, "notes", IndexMode::Semantic).await["ok"], true);
    code.store(401, Ordering::SeqCst);
    let failed = search(root, &["source:notes", "docs"], SearchMode::Hybrid).await;
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(
        failed["result"]["scope_errors"][0]["code"],
        "embedding_paused"
    );
    assert_eq!(failed["result"]["scope_status"][0]["keyword"], "ok");
    assert_eq!(failed["result"]["degraded"], false);
    assert!(!failed["result"]["items"].as_array().unwrap().is_empty());
    code.store(200, Ordering::SeqCst);
    let db = rusqlite::Connection::open(root.join("knowledge.db")).unwrap();
    db.execute("UPDATE spaces SET table_name='wrong_table'", [])
        .unwrap();
    let incompatible = search(root, &["source:notes"], SearchMode::Hybrid).await;
    assert_eq!(incompatible["ok"], false);
    assert_eq!(
        incompatible["result"]["scope_errors"][0]["code"],
        "schema_unsupported"
    );
    assert_eq!(incompatible["result"]["degraded"], false);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn history_without_selected_space_has_history_specific_next_and_unknown_counts() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    config(root, "127.0.0.1:9");
    std::fs::create_dir(root.join("data")).unwrap();
    let db = shellboy::storage::DbWorker::open(root.join("data/shellboy.db"))
        .await
        .unwrap();
    db.shutdown().await.unwrap();
    let found = search(root, &["history"], SearchMode::Hybrid).await;
    assert_eq!(found["ok"], true, "{found}");
    let status = &found["result"]["scope_status"][0];
    assert_eq!(status["reason"], "history_index_not_selected");
    assert!(status["missing_documents"].is_null() && status["missing_chunks"].is_null());
    let command = status["next"]["steps"][0]["command"].as_array().unwrap();
    assert_eq!(
        &command[command.len() - 2..],
        &[json!("index"), json!("status")]
    );
    assert!(!root.join("knowledge.db").exists());
}

#[test]
fn degraded_cli_success_has_one_json_and_zero_exit_status() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    config(root, "127.0.0.1:9");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .current_dir(root)
        .env_clear()
        .arg("--config")
        .arg(root.join("config.toml"))
        .arg("--data-dir")
        .arg(root.join("data"))
        .args(["rag", "--db"])
        .arg(root.join("knowledge.db"))
        .args([
            "search",
            "--scope",
            "docs",
            "--mode",
            "hybrid",
            "--query",
            "readiness",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["result"]["degraded"], true);
    assert!(!root.join("knowledge.db").exists());
}
