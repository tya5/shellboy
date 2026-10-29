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

#[tokio::test]
async fn overlapping_global_thread_and_fork_scopes_do_not_multiply_rrf_votes() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    fixture(root).await;
    let global = search(root, &["history"]).await;
    let one = search(root, &["history:one"]).await;
    let fork = search(root, &["history:fork"]).await;
    for (scopes, originals) in [
        (vec!["history", "history:one"], vec![&global, &one]),
        (vec!["history:one", "history:fork"], vec![&one, &fork]),
    ] {
        let combined = search(root, &scopes).await;
        assert_eq!(combined["ok"], true, "{combined}");
        for hit in combined["result"]["items"].as_array().unwrap() {
            let expected = originals
                .iter()
                .flat_map(|original| original["result"]["items"].as_array().unwrap())
                .filter(|original| original["reference"] == hit["reference"])
                .map(|original| original["score"].as_f64().unwrap())
                .fold(0.0_f64, f64::max);
            assert!(
                (hit["score"].as_f64().unwrap() - expected).abs() < f64::EPSILON,
                "overlapping selectors are one keyword retrieval vote: {hit}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "Synthetic 1k/5k-node Thread keyword timing; run explicitly with --nocapture"]
async fn keyword_thread_scope_thousands_of_nodes_measurement() {
    use std::time::Instant;
    for count in [1000usize, 5000] {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("data");
        std::fs::create_dir(&directory).unwrap();
        let db = DbWorker::open(directory.join("shellboy.db")).await.unwrap();
        db.call(move |db| {
            let tx = db.transaction()?;
            tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('large','/','/a','p','m','now'),('foreign','/','/a','p','m','now');")?;
            let mut previous = None;
            let mut sources = Vec::new();
            let mut groups = 0;
            for index in 0..count {
                let node = format!("original-{index}");
                let text = if index+1 == count { "scope_needle last original".to_owned() } else { format!("synthetic retained original {index}") };
                let payload = json!({"role":"user","content":[{"type":"input_text","text":text}]}).to_string();
                tx.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now',?2)",rusqlite::params![node,payload])?;
                sources.push(node);
                if sources.len() == 128 || index+1 == count {
                    let marker = format!("marker-{groups}");
                    let replacement = format!("replacement-{groups}");
                    let payload = json!({"role":"developer","content":[{"type":"input_text","text":"synthetic omission marker"}]}).to_string();
                    tx.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'event','now',?2)",rusqlite::params![marker,payload])?;
                    tx.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES(?1,?2,?1,'1','1','completed','now')",rusqlite::params![replacement,marker])?;
                    if let Some(previous) = previous.take() { sources.push(previous); }
                    for (ordinal, source) in sources.drain(..).enumerate() {
                        tx.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES(?1,?2,?3,0)",rusqlite::params![replacement,ordinal as i64,source])?;
                    }
                    tx.execute("INSERT INTO thread_replacement(thread_id,replacement_id)VALUES('large',?1)",[&replacement])?;
                    previous = Some(marker);
                    groups += 1;
                }
                let foreign = format!("foreign-{index}");
                let payload = json!({"role":"user","content":[{"type":"input_text","text":"scope_needle foreign"}]}).to_string();
                tx.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now',?2)",rusqlite::params![foreign,payload])?;
                tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('foreign',?1,?2)",rusqlite::params![index as i64,foreign])?;
            }
            tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('large',0,?1)",[previous.unwrap()])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        db.shutdown().await.unwrap();
        let started = Instant::now();
        let found = search(root.path(), &["history:large"]).await;
        let elapsed = started.elapsed();
        assert_eq!(found["ok"], true, "{found}");
        assert_eq!(
            ids(&found),
            BTreeSet::from([format!("original-{}", count - 1)])
        );
        assert_eq!(
            found["result"]["items"][0]["thread_scope"]["on_current_path"],
            false
        );
        println!(
            "THREAD_KEYWORD_MEASUREMENT {}",
            json!({"original_nodes":count,"replacement_layers":count.div_ceil(128),"foreign_nodes":count,"elapsed_ms":elapsed.as_secs_f64()*1000.0,"result_count":1,"embedding_requests":0,"scope":"current_path_plus_recursive_completed_sources","measurement":"full_standalone_keyword_search_per_node_snapshots"})
        );
    }
}
