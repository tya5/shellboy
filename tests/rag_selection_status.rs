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
    let result = call(root, RagCommand::Init).await;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["schema_version"], 3);
    assert_eq!(result["result"]["migrated_from_schema"], Value::Null);
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
        .unwrap_or_else(|| panic!("materials source missing from status: {status}"))
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

// Windows symlink creation may require privileges; this regression uses real
// Unix filesystem links rather than claiming an unavailable Windows result.
#[cfg(unix)]
#[tokio::test]
async fn root_replaced_by_symlink_preserves_all_saved_revisions() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(root.join("materials/old.md"), "original revision").unwrap();
    std::fs::write(root.join("materials/deleted.md"), "also retain").unwrap();
    init(root).await;
    // The root's basename matches, but directory roots are intentionally kept.
    add(root, "materials", &["materials"], false).await;
    assert_eq!(
        index(root, false, IndexMode::Keyword).await["result"]["changed"],
        2
    );
    let before = call(
        root,
        RagCommand::Inspect {
            id: "old.md".into(),
        },
    )
    .await;
    std::fs::rename(root.join("materials"), root.join("moved")).unwrap();
    std::fs::write(root.join("moved/old.md"), "changed behind link").unwrap();
    std::fs::remove_file(root.join("moved/deleted.md")).unwrap();
    std::fs::write(root.join("moved/new.md"), "must not publish").unwrap();
    std::os::unix::fs::symlink(root.join("moved"), root.join("materials")).unwrap();
    for dry_run in [true, false] {
        let unavailable = index(root, dry_run, IndexMode::Keyword).await;
        assert_eq!(unavailable["ok"], false, "{unavailable}");
        assert_eq!(unavailable["error"]["code"], "partial_failure");
        assert_eq!(unavailable["result"]["removed"], 0);
        assert_eq!(unavailable["result"]["processed"], 0);
        assert_eq!(unavailable["result"]["changed"], 0);
        assert_eq!(unavailable["result"]["sources"][0]["scan_complete"], false);
        assert_eq!(
            unavailable["result"]["sources"][0]["error"],
            "source_unavailable"
        );
        let retained = call(
            root,
            RagCommand::Inspect {
                id: "old.md".into(),
            },
        )
        .await;
        assert_eq!(retained["result"]["revision"], before["result"]["revision"]);
        assert_eq!(
            retained["result"]["state"],
            if dry_run { "ready" } else { "stale" }
        );
        assert_eq!(
            call(
                root,
                RagCommand::Inspect {
                    id: "deleted.md".into()
                }
            )
            .await["result"]["state"],
            if dry_run { "ready" } else { "stale" }
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
    }
    // Restore the registered directory and verify normal synchronization resumes.
    std::fs::remove_file(root.join("materials")).unwrap();
    std::fs::rename(root.join("moved"), root.join("materials")).unwrap();
    let recovered = index(root, false, IndexMode::Keyword).await;
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(recovered["result"]["changed"], 2);
    assert_eq!(recovered["result"]["removed"], 1);
}

// Both replacements must be rejected before root-glob matching. In particular,
// no reader may open a FIFO and wait for a writer to produce document bytes.
#[cfg(unix)]
#[tokio::test]
async fn special_file_root_preserves_saved_revisions_before_exclusion() {
    for socket in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let materials = root.join("materials");
        std::fs::create_dir(&materials).unwrap();
        std::fs::write(materials.join("old.md"), "retained original revision").unwrap();
        init(root).await;
        add(root, "materials", &["materials"], false).await;
        assert_eq!(
            index(root, false, IndexMode::Keyword).await["result"]["changed"],
            1
        );
        let before = call(
            root,
            RagCommand::Inspect {
                id: "old.md".into(),
            },
        )
        .await;
        std::fs::remove_dir_all(&materials).unwrap();
        // Keep the socket listener alive throughout both synchronization modes.
        let _listener = if socket {
            Some(std::os::unix::net::UnixListener::bind(&materials).unwrap())
        } else {
            assert!(
                std::process::Command::new("mkfifo")
                    .arg(&materials)
                    .status()
                    .unwrap()
                    .success()
            );
            None
        };
        for dry_run in [true, false] {
            let unavailable = index(root, dry_run, IndexMode::Keyword).await;
            assert_eq!(unavailable["ok"], false, "socket={socket}: {unavailable}");
            assert_eq!(unavailable["error"]["code"], "partial_failure");
            assert_eq!(
                unavailable["result"]["sources"][0]["error"],
                "source_unavailable"
            );
            assert_eq!(unavailable["result"]["sources"][0]["scan_complete"], false);
            assert_eq!(unavailable["result"]["removed"], 0);
            assert_eq!(unavailable["result"]["changed"], 0);
            assert_eq!(unavailable["result"]["processed"], 0);
            let retained = call(
                root,
                RagCommand::Inspect {
                    id: "old.md".into(),
                },
            )
            .await;
            assert_eq!(retained["result"]["revision"], before["result"]["revision"]);
            assert_eq!(
                retained["result"]["state"],
                if dry_run { "ready" } else { "stale" }
            );
        }
        assert_eq!(
            call(root, RagCommand::Status).await["result"]["documents"],
            1
        );
    }
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
    assert_eq!(add(root, "materials/a.md", &[], false).await["ok"], true);
    assert_eq!(
        index(root, false, IndexMode::Keyword).await["result"]["changed"],
        1
    );
    assert_eq!(
        add(root, "materials/a.md", &["*.md"], false).await["ok"],
        true
    );
    for dry_run in [true, false] {
        let excluded_file = index(root, dry_run, IndexMode::Keyword).await;
        assert_eq!(excluded_file["ok"], true);
        assert_eq!(excluded_file["result"]["removed"], 1);
        assert_eq!(
            excluded_file["result"]["sources"][0]["selection"]["targets"],
            0
        );
        assert_eq!(
            excluded_file["result"]["sources"][0]["selection"]["excluded"],
            1
        );
        assert_eq!(
            call(root, RagCommand::Inspect { id: "a.md".into() }).await["result"]["state"],
            if dry_run { "ready" } else { "missing" }
        );
    }
    assert!(
        root.join("materials/a.md").exists(),
        "exclusion must not delete the original file"
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

#[tokio::test]
async fn version_one_readers_reject_without_writes_then_writer_migrates_existing_sources() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(root.join("materials/a.md"), "migration source preserved").unwrap();
    init(root).await;
    let registered = add(root, "materials", &[], false).await;
    assert_eq!(registered["ok"], true, "{registered}");
    let original = registered["result"]["source_id"].clone();
    {
        let db = rusqlite::Connection::open(root.join("rag.db")).unwrap();
        db.execute_batch("DROP INDEX embeddings_vec_rowid; DROP INDEX chunks_hash; ALTER TABLE sources DROP COLUMN exclude_json; PRAGMA user_version=1; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    }
    let before = std::fs::read(root.join("rag.db")).unwrap();
    for result in [
        call(
            root,
            RagCommand::Source {
                command: SourceCommand::List,
            },
        )
        .await,
        call(root, RagCommand::Status).await,
        index(root, true, IndexMode::Keyword).await,
    ] {
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["error"]["code"], "schema_unsupported", "{result}");
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap()
                .contains("shellboy rag init"),
            "{result}"
        );
    }
    assert_eq!(std::fs::read(root.join("rag.db")).unwrap(), before);
    let migrated = call(root, RagCommand::Init).await;
    assert_eq!(migrated["ok"], true, "{migrated}");
    assert_eq!(migrated["result"]["already_initialized"], true);
    assert_eq!(migrated["result"]["schema_version"], 3);
    assert_eq!(migrated["result"]["migrated_from_schema"], 1);
    let reopened = call(root, RagCommand::Init).await;
    assert_eq!(reopened["result"]["migrated_from_schema"], Value::Null);
    assert_eq!(reopened["result"]["schema_version"], 3);
    let listed = call(
        root,
        RagCommand::Source {
            command: SourceCommand::List,
        },
    )
    .await;
    assert_eq!(listed["ok"], true, "{listed}");
    assert!(listed.to_string().contains(original.as_str().unwrap()));
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
    let registered = add(root, "materials", &[], false).await;
    assert_eq!(registered["ok"], true, "{registered}");
    let indexed = index(root, false, IndexMode::Keyword).await;
    assert_eq!(indexed["ok"], true, "{indexed}");
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
    let original_sources: i64 = db
        .query_row("SELECT count(*) FROM sources", [], |r| r.get(0))
        .unwrap();
    assert_eq!(expected.len(), 2);
    db.execute_batch("DROP INDEX embeddings_vec_rowid; DROP INDEX chunks_hash; PRAGMA user_version=2; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    let rejected = call(root, RagCommand::Status).await;
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "schema_unsupported");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let migrated = call(root, RagCommand::Init).await;
    assert_eq!(migrated["ok"], true, "{migrated}");
    assert_eq!(migrated["result"]["migrated_from_schema"], 2);
    assert_eq!(migrated["result"]["schema_version"], 3);
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM sources", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        original_sources
    );
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(indexes(&db), expected);
}

#[tokio::test]
async fn readonly_history_rejects_older_conversation_schema_without_migrating() {
    use shellboy::{rag::SearchMode, storage::DbWorker};
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join("data")).unwrap();
    init(root).await;
    let path = root.join("data/shellboy.db");
    let writer = DbWorker::open(&path).await.unwrap();
    writer.shutdown().await.unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP INDEX shell_active_owner; DROP INDEX run_thread_created; DROP INDEX shell_terminal_time; DROP INDEX draft_state_thread_updated; DROP INDEX draft_submitted_source_revision; ALTER TABLE turn DROP COLUMN interrupt_requested_at; ALTER TABLE receipt DROP COLUMN path_admission; DROP INDEX IF EXISTS thread_parent; DROP INDEX IF EXISTS shell_started_readback; DROP INDEX IF EXISTS shell_owner_started_readback; DROP INDEX IF EXISTS compact_operation_thread; DROP INDEX IF EXISTS automatic_compaction_turn; DELETE FROM schema_migration WHERE version>=10; PRAGMA user_version=9; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    db.close().unwrap();
    let before = std::fs::read(&path).unwrap();
    let result = call(
        root,
        RagCommand::Search {
            query: "synthetic migration".into(),
            scope: vec!["history".into()],
            mode: SearchMode::Keyword,
            limit: 10,
        },
    )
    .await;
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(
        result["result"]["scope_errors"][0]["code"], "schema_unsupported",
        "{result}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let writer = DbWorker::open(&path).await.unwrap();
    writer.shutdown().await.unwrap();
    let result = call(
        root,
        RagCommand::Search {
            query: "synthetic migration".into(),
            scope: vec!["history".into()],
            mode: SearchMode::Keyword,
            limit: 10,
        },
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
}
