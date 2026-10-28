//! Synthetic independent RAG selection, schema compatibility and saved status.
use serde_json::{Value, json};
use shellboy::rag::{IndexMode, RagCommand, RagOptions, SourceCommand, execute};
use std::path::Path;
async fn call(root: &Path, command: RagCommand) -> Value {
    execute(
        RagOptions {
            db: Some(root.join("rag.db")),
            command,
        },
        Some(root.join("missing-config.toml")),
        Some(root.join("data")),
        root,
    )
    .await
}
async fn init(root: &Path) {
    assert_eq!(call(root, RagCommand::Init).await["ok"], true);
}
async fn add(root: &Path, path: &str, exclude: &[&str], clear: bool) -> Value {
    call(
        root,
        RagCommand::Source {
            command: SourceCommand::Add {
                path: path.into(),
                name: "materials".into(),
                exclude: exclude.iter().map(|value| (*value).into()).collect(),
                clear_excludes: clear,
            },
        },
    )
    .await
}
async fn index(root: &Path, dry_run: bool, mode: IndexMode) -> Value {
    call(
        root,
        RagCommand::Index {
            source: Some("materials".into()),
            dry_run,
            mode,
            extractor: None,
            adapter: None,
        },
    )
    .await
}
fn source(status: &Value) -> &Value {
    status["result"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["name"] == "materials")
        .unwrap()
}
#[tokio::test]
async fn persistent_globs_and_default_self_exclusions_do_not_block_removed_documents() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("cache")).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join("a.md"), "retained material").unwrap();
    std::fs::write(root.join("skip.log"), "excluded log").unwrap();
    std::fs::write(root.join("cache/inside.md"), "excluded directory").unwrap();
    std::fs::write(root.join(".git/index"), [0xff, 0xfe]).unwrap();
    std::fs::write(root.join(".DS_Store"), [0xff, 0xfe]).unwrap();
    std::fs::write(root.join("unsupported.bin"), [0xff, 0xfe]).unwrap();
    init(root).await;
    assert_eq!(add(root, ".", &["*.log", "cache"], false).await["ok"], true);
    let preview = index(root, true, IndexMode::Keyword).await;
    assert_eq!(preview["ok"], true, "{preview}");
    let selection = &preview["result"]["sources"][0]["selection"];
    assert_eq!(selection["targets"], 1);
    assert_eq!(selection["unsupported"], 1);
    assert_eq!(selection["unsupported_by_extension"]["bin"], 1);
    assert!(selection["excluded"].as_u64().unwrap() >= 4);
    assert_eq!(preview["result"]["failure_count"], 0);
    assert_eq!(
        index(root, false, IndexMode::Keyword).await["result"]["changed"],
        1
    );
    assert_eq!(
        add(root, ".", &[], false).await["result"]["exclude"],
        json!(["*.log", "cache"])
    );
    std::fs::remove_file(root.join("a.md")).unwrap();
    let removed = index(root, false, IndexMode::Keyword).await;
    assert_eq!(
        removed["result"]["removed"], 1,
        "unsupported files must not freeze deletion detection: {removed}"
    );
    assert_eq!(removed["result"]["failure_count"], 0);
    assert_eq!(
        add(root, ".", &[], true).await["result"]["exclude"],
        json!([])
    );
    assert_eq!(
        index(root, false, IndexMode::Keyword).await["result"]["changed"],
        2
    );
}
#[tokio::test]
async fn newly_excluded_saved_documents_are_removed_and_failure_states_are_accurate() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(
        root.join("materials/old.md"),
        "previous successful revision",
    )
    .unwrap();
    init(root).await;
    add(root, "materials", &[], false).await;
    assert_eq!(index(root, false, IndexMode::Keyword).await["ok"], true);
    std::fs::write(root.join("materials/old.md"), [0xff]).unwrap();
    std::fs::write(root.join("materials/new.md"), [0xff]).unwrap();
    let failed = index(root, false, IndexMode::Keyword).await;
    assert_eq!(failed["result"]["failure_count"], 2);
    let status = call(root, RagCommand::Status).await;
    let saved = source(&status);
    assert_eq!(saved["failed"], 1);
    assert_eq!(saved["stale"], 1);
    assert!(saved["last_scan"].is_string());
    assert!(saved["last_changed_at"].is_string());
    assert!(saved["semantic_missing"].is_null());
    assert!(
        saved["failures"]
            .as_array()
            .unwrap()
            .iter()
            .all(|failure| failure["message"].as_str().unwrap().contains("UTF-8"))
    );
    add(root, "materials", &["*.md"], false).await;
    let excluded = index(root, false, IndexMode::Keyword).await;
    assert_eq!(excluded["result"]["failure_count"], 0);
    assert_eq!(excluded["result"]["removed"], 2);
    let status = call(root, RagCommand::Status).await;
    assert_eq!(source(&status)["failed"], 0);
    assert_eq!(source(&status)["stale"], 0);
    assert!(
        source(&status)["last_changed_at"].is_null(),
        "only active revisions contribute"
    );
}
#[tokio::test]
async fn real_file_failure_and_incomplete_walk_protect_unseen_saved_revisions() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(root.join("materials/old.md"), "previous revision").unwrap();
    init(root).await;
    add(root, "materials", &[], false).await;
    index(root, false, IndexMode::Keyword).await;
    std::fs::remove_file(root.join("materials/old.md")).unwrap();
    std::fs::write(root.join("materials/unreadable.md"), [0xff]).unwrap();
    let protected = index(root, false, IndexMode::Keyword).await;
    assert_eq!(protected["result"]["removed"], 0);
    assert_eq!(protected["result"]["failure_count"], 1);
    std::fs::remove_dir_all(root.join("materials")).unwrap();
    let incomplete = index(root, false, IndexMode::Keyword).await;
    assert_eq!(incomplete["result"]["removed"], 0);
    assert_eq!(incomplete["result"]["sources"][0]["scan_complete"], false);
    assert_eq!(
        call(root, RagCommand::Status).await["result"]["documents"],
        1
    );
}
#[tokio::test]
async fn version_one_read_and_dry_run_are_write_free_then_writer_migrates_existing_sources() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(root.join("materials/a.md"), "migration source preserved").unwrap();
    init(root).await;
    let original = add(root, "materials", &[], false).await["result"]["source_id"].clone();
    {
        let db = rusqlite::Connection::open(root.join("rag.db")).unwrap();
        db.execute_batch("ALTER TABLE sources DROP COLUMN exclude_json; PRAGMA user_version=1; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    }
    let before = std::fs::read(root.join("rag.db")).unwrap();
    assert_eq!(
        call(
            root,
            RagCommand::Source {
                command: SourceCommand::List
            }
        )
        .await["ok"],
        true
    );
    assert_eq!(call(root, RagCommand::Status).await["ok"], true);
    assert_eq!(index(root, true, IndexMode::Keyword).await["ok"], true);
    assert_eq!(std::fs::read(root.join("rag.db")).unwrap(), before);
    let readded = add(root, "materials", &["*.log"], false).await;
    assert_eq!(readded["result"]["source_id"], original);
    let db = rusqlite::Connection::open(root.join("rag.db")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sources WHERE name='materials'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
#[tokio::test]
async fn semantic_preview_reports_segments_without_configuration_or_external_api() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(root.join("materials/a.md"), "meaningful text ".repeat(1000)).unwrap();
    std::fs::write(root.join("materials/b.pdf"), "not executed in preview").unwrap();
    init(root).await;
    add(root, "materials", &[], false).await;
    let preview = index(root, true, IndexMode::Semantic).await;
    assert_eq!(preview["ok"], true, "{preview}");
    assert!(preview["result"]["segments"].as_u64().unwrap() > 0);
    assert!(preview["result"]["text_bytes"].as_u64().unwrap() > 0);
    assert_eq!(preview["result"]["embedding_may_charge"], true);
    assert_eq!(preview["result"]["unmeasured_documents"], 1);
    assert_eq!(preview["result"]["segments_complete"], false);
    assert_eq!(
        call(root, RagCommand::Status).await["result"]["documents"],
        0
    );
}
#[tokio::test]
async fn source_glob_and_sample_bounds_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    for i in 0..30 {
        std::fs::write(root.join(format!("materials/{i}.md")), "text").unwrap();
    }
    init(root).await;
    assert_eq!(
        add(root, "materials", &["[invalid"], false).await["ok"],
        false
    );
    add(root, "materials", &[], false).await;
    let preview = index(root, true, IndexMode::Keyword).await;
    assert_eq!(preview["result"]["sources"][0]["selection"]["targets"], 30);
    assert_eq!(
        preview["result"]["sources"][0]["selection"]["target_examples"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    assert_eq!(
        preview["result"]["sources"][0]["selection"]["examples_limit"],
        20
    );
}

#[tokio::test]
async fn status_uses_current_embedding_profile_and_bounds_saved_failures() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(
        root.join("materials/a.md"),
        "semantic coverage ".repeat(300),
    )
    .unwrap();
    init(root).await;
    add(root, "materials", &[], false).await;
    index(root, false, IndexMode::Keyword).await;
    std::fs::write(root.join("missing-config.toml"), "[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n[embedding]\nconnection='fixture'\nmodel='offline-profile'\n").unwrap();
    let status = call(root, RagCommand::Status).await;
    assert_eq!(
        status["ok"], true,
        "status must not contact the offline embedding API: {status}"
    );
    assert_eq!(
        status["result"]["semantic_configuration"]["state"],
        "configured"
    );
    assert!(source(&status)["semantic_missing"].as_i64().unwrap() > 0);
    let db = rusqlite::Connection::open(root.join("rag.db")).unwrap();
    let changed: Option<String> = db.query_row("SELECT MAX(r.created_at) FROM revisions r JOIN documents d ON d.active_revision=r.id JOIN sources s ON s.id=d.source_id WHERE s.name='materials'", [], |row| row.get(0)).unwrap();
    assert_eq!(source(&status)["last_changed_at"], json!(changed));
    drop(db);
    for i in 0..60 {
        std::fs::write(root.join(format!("materials/bad-{i}.md")), [0xff]).unwrap();
    }
    assert_eq!(
        index(root, false, IndexMode::Keyword).await["result"]["failure_count"],
        60
    );
    let status = call(root, RagCommand::Status).await;
    assert_eq!(source(&status)["failure_count"], 60);
    assert_eq!(source(&status)["failures"].as_array().unwrap().len(), 50);
    assert_eq!(source(&status)["failures_truncated"], true);
    assert_eq!(source(&status)["state"], "partial_failure");
}
