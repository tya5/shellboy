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
async fn complete_walk_removes_unseen_documents_despite_file_failure() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(root.join("materials/deleted.md"), "deleted revision").unwrap();
    std::fs::write(root.join("materials/stale.md"), "retained revision").unwrap();
    init(root).await;
    add(root, "materials", &[], false).await;
    index(root, false, IndexMode::Keyword).await;
    let before = call(
        root,
        RagCommand::Inspect {
            id: "stale.md".into(),
        },
    )
    .await;
    std::fs::remove_file(root.join("materials/deleted.md")).unwrap();
    std::fs::write(root.join("materials/stale.md"), [0xff]).unwrap();
    std::fs::write(root.join("materials/new.md"), [0xff]).unwrap();
    let preview = index(root, true, IndexMode::Keyword).await;
    assert_eq!(preview["result"]["removed"], 1, "{preview}");
    assert_eq!(preview["result"]["failure_count"], 2);
    assert_eq!(
        call(
            root,
            RagCommand::Inspect {
                id: "stale.md".into()
            }
        )
        .await["result"],
        before["result"],
        "preview must not change saved revision or state"
    );
    for expected_removed in [1, 0] {
        let partial = index(root, false, IndexMode::Keyword).await;
        assert_eq!(partial["result"]["removed"], expected_removed, "{partial}");
        assert_eq!(partial["result"]["failure_count"], 2);
        assert_eq!(partial["result"]["sources"][0]["deletion_detection"], true);
        let stale = call(
            root,
            RagCommand::Inspect {
                id: "stale.md".into(),
            },
        )
        .await;
        assert_eq!(stale["result"]["state"], "stale");
        assert_eq!(stale["result"]["revision"], before["result"]["revision"]);
        assert_eq!(
            call(
                root,
                RagCommand::Inspect {
                    id: "deleted.md".into()
                }
            )
            .await["result"]["state"],
            "missing"
        );
        assert_eq!(
            call(
                root,
                RagCommand::Inspect {
                    id: "new.md".into()
                }
            )
            .await["result"]["state"],
            "failed"
        );
    }
}

#[tokio::test]
async fn incomplete_walk_defers_updates_and_preserves_unseen_saved_revisions() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(root.join("materials/old.md"), "previous revision").unwrap();
    init(root).await;
    add(root, "materials", &[], false).await;
    index(root, false, IndexMode::Keyword).await;
    let before = call(
        root,
        RagCommand::Inspect {
            id: "old.md".into(),
        },
    )
    .await;
    // The deterministic depth cap makes traversal fail after seeing a changed
    // early file; the whole walk must finish before any revision is published.
    std::fs::write(root.join("materials/old.md"), "changed revision").unwrap();
    std::fs::write(root.join("materials/new.md"), "new revision").unwrap();
    let mut deep = root.join("materials/z");
    std::fs::create_dir(&deep).unwrap();
    for _ in 0..65 {
        deep = deep.join("d");
        std::fs::create_dir(&deep).unwrap();
    }
    let incomplete = index(root, false, IndexMode::Keyword).await;
    assert_eq!(incomplete["result"]["removed"], 0);
    assert_eq!(incomplete["result"]["processed"], 0);
    assert_eq!(incomplete["result"]["sources"][0]["scan_complete"], false);
    assert_eq!(
        call(
            root,
            RagCommand::Inspect {
                id: "old.md".into()
            }
        )
        .await["result"]["revision"],
        before["result"]["revision"]
    );
    assert_eq!(
        call(
            root,
            RagCommand::Inspect {
                id: "new.md".into()
            }
        )
        .await["ok"],
        false
    );
    std::fs::remove_dir_all(root.join("materials")).unwrap();
    let missing_root = index(root, false, IndexMode::Keyword).await;
    assert_eq!(missing_root["result"]["removed"], 0);
    assert_eq!(missing_root["result"]["sources"][0]["scan_complete"], false);
    assert_eq!(
        call(root, RagCommand::Status).await["result"]["documents"],
        1
    );
}

#[tokio::test]
async fn exclusions_skip_descendants_but_do_not_exclude_directory_source_root() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir_all(root.join("materials/materials")).unwrap();
    std::fs::write(root.join("materials/a.md"), "keep").unwrap();
    std::fs::write(root.join("materials/materials/nested.md"), "exclude").unwrap();
    init(root).await;
    add(root, "materials", &[], false).await;
    index(root, false, IndexMode::Keyword).await;
    add(root, "materials", &["materials"], false).await;
    for dry_run in [true, false] {
        let selected = index(root, dry_run, IndexMode::Keyword).await;
        assert_eq!(selected["result"]["sources"][0]["selection"]["targets"], 1);
        assert_eq!(selected["result"]["removed"], 1);
        assert_eq!(selected["result"]["unchanged"], 1);
    }
    assert_eq!(
        call(root, RagCommand::Inspect { id: "a.md".into() }).await["result"]["state"],
        "ready"
    );
    // An explicitly registered file, unlike a directory root, can be excluded.
    assert_eq!(
        call(
            root,
            RagCommand::Source {
                command: SourceCommand::Remove {
                    source: "materials".into()
                }
            }
        )
        .await["ok"],
        true
    );
    assert_eq!(
        add(root, "materials/a.md", &["*.md"], false).await["ok"],
        true
    );
    let excluded_file = index(root, true, IndexMode::Keyword).await;
    assert_eq!(
        excluded_file["result"]["sources"][0]["selection"]["targets"],
        0
    );
    assert_eq!(
        excluded_file["result"]["sources"][0]["selection"]["excluded"],
        1
    );
}

#[tokio::test]
async fn glob_case_sensitivity_is_independent_of_filesystem_case_rules() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(root.join("materials/a.log"), "keep").unwrap();
    init(root).await;
    add(root, "materials", &["*.LOG"], false).await;
    assert_eq!(
        index(root, true, IndexMode::Keyword).await["result"]["sources"][0]["selection"]["targets"],
        1
    );
    add(root, "materials", &["*.log"], false).await;
    assert_eq!(
        index(root, true, IndexMode::Keyword).await["result"]["sources"][0]["selection"]["targets"],
        0
    );
}

#[cfg(windows)]
#[tokio::test]
async fn windows_legacy_backslash_key_keeps_document_identity_across_sync() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir_all(root.join("materials/sub")).unwrap();
    std::fs::write(root.join("materials/sub/a.md"), "stable material").unwrap();
    init(root).await;
    add(root, "materials", &[], false).await;
    index(root, false, IndexMode::Keyword).await;
    // Model the key representation stored by the pre-normalization scanner.
    let db = rusqlite::Connection::open(root.join("rag.db")).unwrap();
    db.execute(
        "UPDATE documents SET key='sub\\a.md' WHERE key='sub/a.md'",
        [],
    )
    .unwrap();
    drop(db);
    let before = call(
        root,
        RagCommand::Inspect {
            id: "sub\\a.md".into(),
        },
    )
    .await;
    assert_eq!(before["ok"], true);
    for dry_run in [true, false] {
        let synced = index(root, dry_run, IndexMode::Keyword).await;
        assert_eq!(synced["result"]["removed"], 0, "{synced}");
        assert_eq!(synced["result"]["changed"], 0);
        assert_eq!(synced["result"]["unchanged"], 1);
        let saved = call(
            root,
            RagCommand::Inspect {
                id: "sub\\a.md".into(),
            },
        )
        .await;
        assert_eq!(
            saved["result"]["document_id"],
            before["result"]["document_id"]
        );
        assert_eq!(saved["result"]["revision"], before["result"]["revision"]);
    }
    // Glob matching still uses slash paths even when storage uses an old key.
    add(root, "materials", &["sub/*.md"], false).await;
    assert_eq!(
        index(root, false, IndexMode::Keyword).await["result"]["removed"],
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
        db.execute_batch("DROP INDEX embeddings_vec_rowid; DROP INDEX chunks_hash; ALTER TABLE sources DROP COLUMN exclude_json; PRAGMA user_version=1; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
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
        3
    );
    assert_eq!(db.query_row("SELECT count(*) FROM sqlite_schema WHERE type='index' AND name IN ('embeddings_vec_rowid','chunks_hash')", [], |row| row.get::<_,i64>(0)).unwrap(), 2);
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
    assert_eq!(preview["result"]["unmeasured_documents"], 0);
    assert_eq!(preview["result"]["segments_complete"], true);
    assert_eq!(
        preview["result"]["sources"][0]["selection"]["unsupported"],
        1
    );
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

#[tokio::test]
async fn version_two_migration_adds_same_hot_indexes_as_fresh_store_without_touching_readers() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    init(root).await;
    let path = root.join("rag.db");
    let indexes = |db: &rusqlite::Connection| -> Vec<(String, String)> {
        db.prepare("SELECT name,sql FROM sqlite_schema WHERE type='index' AND name IN ('embeddings_vec_rowid','chunks_hash') ORDER BY name").unwrap().query_map([],|row|Ok((row.get(0)?,row.get(1)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
    };
    let db = rusqlite::Connection::open(&path).unwrap();
    let expected = indexes(&db);
    assert_eq!(expected.len(), 2);
    db.execute_batch("DROP INDEX embeddings_vec_rowid; DROP INDEX chunks_hash; PRAGMA user_version=2; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(call(root, RagCommand::Status).await["ok"], true);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::create_dir(root.join("notes")).unwrap();
    assert_eq!(add(root, "notes", &[], false).await["ok"], true);
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(indexes(&db), expected);
}
