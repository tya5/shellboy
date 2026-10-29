//! Thread-scoped keyword history needs no embedding, owner, or index.
use serde_json::{Value, json};
use shellboy::{
    rag::{RagCommand, RagOptions, SearchMode, execute},
    storage::DbWorker,
};
use std::{collections::BTreeSet, path::Path};

async fn fixture(root: &Path) {
    std::fs::create_dir(root.join("data")).unwrap();
    let db = DbWorker::open(root.join("data/shellboy.db")).await.unwrap();
    db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('one','/','/AGENTS.md','unused','unused','now'),('fork','/','/AGENTS.md','unused','unused','now'),('gone','/','/AGENTS.md','unused','unused','now'); UPDATE thread SET parent_thread_id='one' WHERE id='fork'; UPDATE thread SET deleted_at='now' WHERE id='gone';")?;
        for (id, kind) in [("current","user"),("omit","event"),("nested","summary"),("original","user"),("foreign","user"),("pending","user")] {
            let payload = json!({"role":"developer","content":[{"type":"input_text","text":format!("scope_needle {id}")}]});
            db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,?2,'now',?3)",rusqlite::params![id,kind,payload.to_string()])?;
        }
        db.execute_batch("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('one',0,'current'),('one',1,'omit'),('fork',0,'current'),('fork',1,'foreign'),('gone',0,'pending');
            INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES('omission','omit','omit-key','1','1','completed','now'),('nested-summary','nested','summary-key','1','1','completed','now'),('not-finished','omit','pending-key','1','1','pending','now');
            INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES('omission',0,'nested',0),('nested-summary',0,'original',0),('not-finished',0,'pending',0);
            INSERT INTO thread_replacement(thread_id,replacement_id)VALUES('one','omission'),('one','nested-summary');")?;
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
}

async fn search(root: &Path, scopes: &[&str]) -> Value {
    execute(
        RagOptions {
            db: Some(root.join("absent-knowledge.db")),
            command: RagCommand::Search {
                query: "scope_needle".into(),
                scope: scopes.iter().map(|scope| (*scope).into()).collect(),
                mode: SearchMode::Keyword,
                limit: 100,
            },
        },
        Some(root.join("missing-config.toml")),
        Some(root.join("data")),
        root,
    )
    .await
}

fn ids(found: &Value) -> BTreeSet<String> {
    found["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["location"]["node_id"].as_str().unwrap().into())
        .collect()
}

#[tokio::test]
async fn scoped_keyword_recovers_transitive_omitted_originals_without_embedding_or_writes() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    fixture(root).await;
    let before = std::fs::read(root.join("data/shellboy.db")).unwrap();
    let found = search(root, &["history:one"]).await;
    assert_eq!(found["ok"], true, "{found}");
    assert_eq!(
        ids(&found),
        BTreeSet::from([
            "current".into(),
            "omit".into(),
            "nested".into(),
            "original".into()
        ])
    );
    for hit in found["result"]["items"].as_array().unwrap() {
        let id = hit["location"]["node_id"].as_str().unwrap();
        assert_eq!(hit["thread_scope"]["thread_id"], "one");
        assert_eq!(
            hit["thread_scope"]["on_current_path"],
            matches!(id, "current" | "omit")
        );
        if matches!(id, "nested" | "original") {
            assert_eq!(hit["provenance"]["current_thread_paths"], json!([]));
        }
    }
    let global = search(root, &["history"]).await;
    assert_eq!(global["ok"], true, "{global}");
    assert!(ids(&global).contains("foreign"));
    assert!(
        global["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| hit.get("thread_scope").is_none())
    );
    let original = found["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["location"]["node_id"] == "original")
        .unwrap();
    let body = execute(
        RagOptions {
            db: Some(root.join("absent-knowledge.db")),
            command: RagCommand::Get {
                reference: original["reference"].as_str().unwrap().into(),
                limit_bytes: 16384,
                cursor: None,
            },
        },
        Some(root.join("missing-config.toml")),
        Some(root.join("data")),
        root,
    )
    .await;
    assert_eq!(body["ok"], true, "{body}");
    assert!(
        body["result"]["text"]
            .as_str()
            .unwrap()
            .contains("scope_needle original")
    );
    assert_eq!(
        std::fs::read(root.join("data/shellboy.db")).unwrap(),
        before
    );
    assert!(!root.join("absent-knowledge.db").exists());
    assert!(!root.join("missing-config.toml").exists());
}

#[tokio::test]
async fn shared_reference_keeps_each_explicit_thread_membership_and_bad_scopes_fail() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    fixture(root).await;
    let found = search(root, &["history:one", "history:fork"]).await;
    assert_eq!(found["ok"], true, "{found}");
    let shared = found["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["location"]["node_id"] == "current")
        .unwrap();
    assert_eq!(
        shared["thread_scopes"],
        json!([{"thread_id":"one","on_current_path":true},{"thread_id":"fork","on_current_path":true}])
    );
    assert_eq!(shared["scopes"], json!(["history:one", "history:fork"]));
    assert!(shared.get("thread_scope").is_none());
    for (scope, code) in [
        ("history:", "scope_invalid"),
        ("history:missing", "thread_not_found"),
        ("history:gone", "thread_not_found"),
    ] {
        let failed = search(root, &[scope]).await;
        assert_eq!(failed["ok"], false, "{failed}");
        assert_eq!(
            failed["result"]["scope_errors"][0]["code"], code,
            "{failed}"
        );
        assert_eq!(failed["result"]["items"], json!([]));
    }
}
