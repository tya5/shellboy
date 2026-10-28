//! Anonymous knowledge stores and loopback embedding, never user data.
use serde_json::{Value, json};
use shellboy::rag::{IndexMode, RagCommand, RagOptions, SearchMode, SourceCommand, execute};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};

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
async fn init(root: &Path) {
    assert_eq!(call(root, RagCommand::Init).await["ok"], true);
}
async fn add(root: &Path, name: &str, path: &str) {
    assert_eq!(
        call(
            root,
            RagCommand::Source {
                command: SourceCommand::Add {
                    exclude: vec![],
                    clear_excludes: false,
                    path: path.into(),
                    name: name.into()
                }
            }
        )
        .await["ok"],
        true
    );
}
async fn index(root: &Path, source: Option<&str>, mode: IndexMode) -> Value {
    call(
        root,
        RagCommand::Index {
            source: source.map(str::to_owned),
            dry_run: false,
            mode,
        },
    )
    .await
}
async fn search(root: &Path, scope: &str, query: &str, mode: SearchMode) -> Value {
    call(
        root,
        RagCommand::Search {
            query: query.into(),
            scope: vec![scope.into()],
            mode,
            limit: 10,
        },
    )
    .await
}
async fn proxy(
    root: &Path,
) -> (
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    proxy_with_gate(root, None).await
}

// Only marked query requests are held. Each arrival owns its response release;
// dropping the test-side sender closes that request instead of orphaning it.
async fn held_query(arrivals: &mut mpsc::Receiver<oneshot::Sender<()>>) -> oneshot::Sender<()> {
    tokio::time::timeout(std::time::Duration::from_secs(5), arrivals.recv())
        .await
        .expect("marked query must arrive at the HTTP response gate")
        .expect("HTTP fixture must retain the query arrival sender")
}

async fn proxy_with_gate(
    root: &Path,
    gate: Option<mpsc::Sender<oneshot::Sender<()>>>,
) -> (
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(root.join("config.toml"),format!("[connections.fixture]\nbase_url='http://{address}/v1'\n[embedding]\nconnection='fixture'\nmodel='synthetic'\n")).unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let fail_at = Arc::new(AtomicUsize::new(usize::MAX));
    let captured = requests.clone();
    let failing = fail_at.clone();
    let task = tokio::spawn(async move {
        // The listener owns all connection tasks. Aborting/joining this fixture
        // drops the JoinSet and cancels any unreleased response gates as well.
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                finished = connections.join_next(), if !connections.is_empty() => {
                    finished.expect("tracked fixture request").expect("fixture request must not panic");
                    continue;
                }
            };
            let Ok((mut stream, _)) = accepted else {
                return;
            };
            let requests = captured.clone();
            let failing = failing.clone();
            let gate = gate.clone();
            connections.spawn(async move {
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut b = [0; 4096];
                    let n = stream.read(&mut b).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&b[..n]);
                    if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|s| {
                        s.strip_prefix("content-length:")
                            .map(|s| s.trim().parse().unwrap())
                    })
                    .unwrap();
                while bytes.len() < header_end + length {
                    let mut b = [0; 4096];
                    let n = stream.read(&mut b).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&b[..n]);
                }
                let request: Value =
                    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                let count = requests.fetch_add(1, Ordering::SeqCst) + 1;
                let held = request["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| {
                        let input = s.as_str().unwrap();
                        input.contains("DELAY") || input.contains("SHUTDOWN")
                    });
                if held {
                    let gate = gate.as_ref().expect("marked query requires an explicit test gate");
                    let (release, released) = oneshot::channel();
                    if gate.send(release).await.is_err() || released.await.is_err() {
                        return;
                    }
                }
                let limited = failing.load(Ordering::SeqCst) == usize::MAX - 1;
                let alternate = failing.load(Ordering::SeqCst) == usize::MAX - 2 && count % 2 == 1;
                let always_fail = failing.load(Ordering::SeqCst) == usize::MAX - 3;
                let (failure, body) = if failing.load(Ordering::SeqCst) == count
                    || limited
                    || alternate
                    || always_fail
                {
                    (true, json!({"error":{"message":"synthetic failure"}}))
                } else {
                    let data=request["input"].as_array().unwrap().iter().enumerate().map(|(i,t)|json!({"index":i,"embedding":if t.as_str().unwrap().contains("NEAR"){vec![1.0,0.0]}else{vec![0.0,1.0]}})).collect::<Vec<_>>();
                    (false, json!({"data":data}))
                };
                let body = body.to_string();
                let status = if limited {
                    "429 Too Many Requests"
                } else if failure {
                    "500 Synthetic"
                } else {
                    "200 OK"
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (requests, fail_at, task)
}

#[tokio::test]
async fn semantic_cache_reuses_successful_content_and_unregister_prunes_vectors() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (requests, _, server) = proxy(root).await;
    init(root).await;
    std::fs::write(root.join("near.md"), "NEAR 復旧の一次資料").unwrap();
    std::fs::write(root.join("far.md"), "FAR 別の資料").unwrap();
    add(root, "near", "near.md").await;
    add(root, "far", "far.md").await;
    assert_eq!(index(root, None, IndexMode::Semantic).await["ok"], true);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert_eq!(index(root, None, IndexMode::Semantic).await["ok"], true);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    let found = search(root, "source:near", "NEAR", SearchMode::Hybrid).await;
    assert_eq!(found["ok"], true, "{found}");
    assert_eq!(found["result"]["items"][0]["method"], "hybrid");
    assert_eq!(found["result"]["items"][0]["distance"], 0.0);
    let reference = found["result"]["items"][0]["reference"]
        .as_str()
        .unwrap()
        .to_owned();
    let got = call(
        root,
        RagCommand::Get {
            reference: reference.clone(),
            limit_bytes: 65536,
            cursor: None,
        },
    )
    .await;
    assert_eq!(got["result"]["text"], "NEAR 復旧の一次資料");
    assert_eq!(
        call(
            root,
            RagCommand::Source {
                command: SourceCommand::Remove {
                    source: "near".into()
                }
            }
        )
        .await["ok"],
        true
    );
    let db = rusqlite::Connection::open(root.join("knowledge.db")).unwrap();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert!(root.join("near.md").is_file());
    assert_eq!(
        call(
            root,
            RagCommand::Get {
                reference,
                limit_bytes: 65536,
                cursor: None
            }
        )
        .await["error"]["code"],
        "revision_unavailable"
    );
    server.abort();
}

#[tokio::test]
async fn failed_embedding_keeps_previous_version_and_retry_reuses_successful_batches() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (requests, fail_at, server) = proxy(root).await;
    init(root).await;
    std::fs::write(root.join("notes.md"), "OLD 復旧").unwrap();
    add(root, "notes", "notes.md").await;
    assert_eq!(index(root, None, IndexMode::Semantic).await["ok"], true);
    let body = (0..17)
        .map(|i| format!("NEW-{i:02} {}", "a".repeat(1988)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(root.join("notes.md"), body).unwrap();
    fail_at.store(requests.load(Ordering::SeqCst) + 2, Ordering::SeqCst);
    let failed = index(root, None, IndexMode::Semantic).await;
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(
        failed["result"]["changed"], 1,
        "attempted change is counted before embedding failure"
    );
    assert_eq!(failed["result"]["unchanged"], 0);
    assert_eq!(
        failed["result"]["processed"], 0,
        "publication failed; old revision remains current"
    );
    let old = search(root, "source:notes", "OLD", SearchMode::Keyword).await;
    assert_eq!(old["result"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(old["result"]["items"][0]["provenance"]["state"], "stale");
    let before = requests.load(Ordering::SeqCst);
    fail_at.store(usize::MAX, Ordering::SeqCst);
    let retried = index(root, None, IndexMode::Semantic).await;
    assert_eq!(retried["ok"], true, "{retried}");
    assert_eq!(retried["result"]["changed"], 1);
    assert_eq!(retried["result"]["processed"], 1);
    assert!(
        requests.load(Ordering::SeqCst) - before <= 2,
        "saved first batch must be reused"
    );
    assert!(
        search(root, "source:notes", "OLD", SearchMode::Keyword).await["result"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    server.abort();
}

#[tokio::test]
async fn keyword_crosses_chunk_boundaries_and_cursor_cannot_cross_references() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    init(root).await;
    let text = format!("{}復旧または再開", "あ".repeat(1999));
    std::fs::write(root.join("long.md"), &text).unwrap();
    add(root, "long", "long.md").await;
    assert_eq!(index(root, None, IndexMode::Keyword).await["ok"], true);
    let found = search(root, "source:long", "復旧", SearchMode::Keyword).await;
    assert_eq!(found["result"]["items"].as_array().unwrap().len(), 1);
    let reference = found["result"]["items"][0]["reference"]
        .as_str()
        .unwrap()
        .to_owned();
    let page = call(
        root,
        RagCommand::Get {
            reference: reference.clone(),
            limit_bytes: 7,
            cursor: None,
        },
    )
    .await;
    assert_eq!(page["result"]["text"], "ああ");
    let mut raw = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        &reference,
    )
    .unwrap();
    let mut witness: Value = serde_json::from_slice(&raw).unwrap();
    witness["id"] = json!("another-document");
    raw = serde_json::to_vec(&witness).unwrap();
    let other = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw);
    let bad = call(
        root,
        RagCommand::Get {
            reference: other,
            limit_bytes: 7,
            cursor: page["result"]["next_cursor"].as_str().map(str::to_owned),
        },
    )
    .await;
    assert_eq!(bad["ok"], false);
}

#[tokio::test]
async fn readonly_history_semantic_reuses_index_and_rejects_space_change_and_tombstone() {
    use sha2::{Digest, Sha256};
    use shellboy::{config::Config, index::IndexService, storage::DbWorker};
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (gate, mut arrivals) = mpsc::channel(1);
    let (_, _, server) = proxy_with_gate(root, Some(gate)).await;
    std::fs::create_dir(root.join("data")).unwrap();
    let db = DbWorker::open(root.join("data/shellboy.db")).await.unwrap();
    let config =
        Config::parse(&std::fs::read_to_string(root.join("config.toml")).unwrap()).unwrap();
    let payload = json!({"role":"user","content":[{"type":"input_text","text":"NEAR 会話の復旧"}]})
        .to_string();
    let payload_hash = Sha256::digest(payload.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    db.call(move|c|{c.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES('history-node','user','2026-01-01T00:00:00Z',?1,?2,?3)",rusqlite::params![payload,payload.len()as i64,payload_hash])?;Ok(())}).await.unwrap();
    let index = IndexService::new(db.clone(), Arc::new(config));
    index.enqueue_new(&["history-node".into()]).await.unwrap();
    while index
        .process_one(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap()
    {}
    let found = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(found["ok"], true, "{found}");
    assert_eq!(found["result"]["items"][0]["distance"], 0.0);
    assert_eq!(
        found["result"]["items"][0]["provenance"]["current_thread_paths"],
        json!([])
    );
    let reference = found["result"]["items"][0]["reference"]
        .as_str()
        .unwrap()
        .to_owned();
    let got = call(
        root,
        RagCommand::Get {
            reference: reference.clone(),
            limit_bytes: 65536,
            cursor: None,
        },
    )
    .await;
    assert_eq!(got["result"]["text"], "NEAR 会話の復旧");
    let path = root.to_path_buf();
    let pending =
        tokio::spawn(
            async move { search(&path, "history", "DELAY NEAR", SearchMode::Semantic).await },
        );
    let release = held_query(&mut arrivals).await;
    db.call(|c| {
        c.execute(
            "UPDATE app_meta SET value='removed-space' WHERE key='index.active_space'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        !pending.is_finished(),
        "space changed while query response was held"
    );
    release
        .send(())
        .expect("query must still await response release");
    let changed = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .expect("released history query must finish")
        .unwrap();
    assert_eq!(changed["ok"], false);
    assert_eq!(
        changed["result"]["scope_errors"][0]["code"],
        "history_changed"
    );
    db.call(|c|{c.execute("UPDATE node SET payload_json=NULL,payload_revision=payload_revision+1,deleted_reason='fixture collected' WHERE id='history-node'",[])?;Ok(())}).await.unwrap();
    let missing = call(
        root,
        RagCommand::Get {
            reference,
            limit_bytes: 65536,
            cursor: None,
        },
    )
    .await;
    assert_eq!(missing["error"]["code"], "revision_unavailable");
    db.shutdown().await.unwrap();
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn material_query_releases_snapshot_during_api_and_rejects_new_publication() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (gate, mut arrivals) = mpsc::channel(1);
    let (_, _, server) = proxy_with_gate(root, Some(gate)).await;
    init(root).await;
    std::fs::write(root.join("notes.md"), "OLD NEAR").unwrap();
    add(root, "notes", "notes.md").await;
    assert_eq!(index(root, None, IndexMode::Semantic).await["ok"], true);
    let task_root = root.to_path_buf();
    let pending = tokio::spawn(async move {
        search(
            &task_root,
            "source:notes",
            "DELAY NEAR",
            SearchMode::Semantic,
        )
        .await
    });
    let release = held_query(&mut arrivals).await;
    std::fs::write(root.join("notes.md"), "NEW unembedded revision").unwrap();
    assert_eq!(index(root, None, IndexMode::Keyword).await["ok"], true);
    let db = rusqlite::Connection::open(root.join("knowledge.db")).unwrap();
    let busy: i64 = db
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
        .unwrap();
    assert_eq!(busy, 0, "network await must not retain a read snapshot");
    assert!(
        !pending.is_finished(),
        "publication and checkpoint completed while response was held"
    );
    release
        .send(())
        .expect("query must still await response release");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .expect("released material query must finish")
        .unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(
        result["result"]["scope_errors"][0]["code"],
        "source_changed"
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn concurrent_writer_is_bounded_busy_and_short_keywords_survive_partial_file_failure() {
    use fs2::FileExt;
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    init(root).await;
    std::fs::create_dir(root.join("notes")).unwrap();
    std::fs::write(root.join("notes/good.md"), "復旧 GOOD").unwrap();
    add(root, "notes", "notes").await;
    assert_eq!(index(root, None, IndexMode::Keyword).await["ok"], true);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("knowledge.rag-lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let busy = index(root, None, IndexMode::Keyword).await;
    assert_eq!(busy["error"]["code"], "busy");
    drop(lock);
    std::fs::write(root.join("notes/bad.md"), [0xff, 0xfe, 0xfd]).unwrap();
    let partial = index(root, None, IndexMode::Keyword).await;
    assert_eq!(partial["ok"], false);
    assert_eq!(partial["result"]["failure_count"], 1);
    assert_eq!(
        search(root, "source:notes", "復旧", SearchMode::Keyword).await["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn provider_rate_limit_stops_the_invocation_and_retry_preserves_old_and_cached_work() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (requests, fail_at, server) = proxy(root).await;
    init(root).await;
    std::fs::create_dir(root.join("notes")).unwrap();
    for i in 0..4 {
        std::fs::write(root.join(format!("notes/{i}.md")), format!("OLD-{i}")).unwrap();
    }
    add(root, "notes", "notes").await;
    assert_eq!(index(root, None, IndexMode::Keyword).await["ok"], true);
    for i in 0..4 {
        std::fs::write(root.join(format!("notes/{i}.md")), format!("NEW-{i} NEAR")).unwrap();
    }
    fail_at.store(usize::MAX - 1, Ordering::SeqCst);
    let failed = index(root, None, IndexMode::Semantic).await;
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["result"]["embedding_paused"], true);
    assert_eq!(failed["result"]["deferred"], 3);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "429 must not be retried per file"
    );
    let previous = search(root, "source:notes", "OLD", SearchMode::Keyword).await;
    assert_eq!(previous["result"]["items"].as_array().unwrap().len(), 4);
    fail_at.store(usize::MAX, Ordering::SeqCst);
    assert_eq!(index(root, None, IndexMode::Semantic).await["ok"], true);
    let before = requests.load(Ordering::SeqCst);
    assert_eq!(index(root, None, IndexMode::Semantic).await["ok"], true);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        before,
        "successful retry embeddings must be cached"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_shuts_down_normally_during_history_query_http_and_query_reopens_offline() {
    use sha2::{Digest, Sha256};
    use shellboy::{app::DataOwner, index::IndexService};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (gate, mut arrivals) = mpsc::channel(1);
    let (_, _, server) = proxy_with_gate(root, Some(gate)).await;
    let owner = DataOwner::start(
        Some(root.join("config.toml")),
        Some(root.join("data")),
        root.into(),
    )
    .await
    .unwrap();
    owner.db.call(|db| {
        let payload = json!({"role":"user","content":[{"type":"input_text","text":"NEAR shutdown fixture"}]}).to_string();
        let content_hash = Sha256::digest(payload.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>();
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES('shutdown-node','user','now',?1,?2,?3)", rusqlite::params![payload,payload.len() as i64,content_hash])?;
        Ok(())
    }).await.unwrap();
    let index = IndexService::new(owner.db.clone(), Arc::new(owner.loaded.config.clone()));
    index.enqueue_new(&["shutdown-node".into()]).await.unwrap();
    while index
        .process_one(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap()
    {}
    let task_root = root.to_path_buf();
    let pending = tokio::spawn(async move {
        search(&task_root, "history", "SHUTDOWN NEAR", SearchMode::Semantic).await
    });
    let release = held_query(&mut arrivals).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), owner.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !pending.is_finished(),
        "query HTTP fixture must still be in flight at owner shutdown"
    );
    release
        .send(())
        .expect("owner shutdown must not cancel independent query HTTP");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .expect("released shutdown query must finish")
        .unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["items"].as_array().unwrap().len(), 1);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn history_keyword_hides_deleted_thread_exclusive_nodes_before_physical_gc() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::write(root.join("config.toml"), "").unwrap();
    std::fs::create_dir(root.join("data")).unwrap();
    let db = shellboy::storage::DbWorker::open(root.join("data/shellboy.db"))
        .await
        .unwrap();
    db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('dead','/','/a','p','m','now'),('live','/','/a','p','m','now');")?;
        for id in ["deleted-only","source","summary"] {
            db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now',?2)",rusqlite::params![id,json!({"role":"user","content":"visibility_needle"}).to_string()])?;
        }
        db.execute_batch("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('dead',0,'deleted-only'),('dead',1,'source'),('live',0,'summary'); INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES('r','summary','k','1','1','completed','now'); INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES('r',0,'source',0);")?;
        Ok(())
    }).await.unwrap();
    let store = shellboy::control::db::Store::new(
        db.clone(),
        Arc::new(shellboy::config::Config::default()),
        root.into(),
        "keyword-delete".into(),
    );
    let before = search(root, "history", "visibility_needle", SearchMode::Keyword).await;
    assert_eq!(before["ok"], true, "{before}");
    assert_eq!(before["result"]["items"].as_array().unwrap().len(), 3);
    store
        .execute(
            shellboy::protocol::Request::new(
                "keyword-delete:dead",
                shellboy::protocol::Command::ThreadDelete(shellboy::protocol::ThreadDeleteArgs {
                    thread_id: "dead".into(),
                    descendants: false,
                }),
            ),
            None,
        )
        .await
        .unwrap();
    let found = search(root, "history", "visibility_needle", SearchMode::Keyword).await;
    assert_eq!(found["ok"], true, "{found}");
    let mut ids = found["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["location"]["node_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec!["source", "summary"]);
    db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            3
        );
        Ok(())
    })
    .await
    .unwrap();
    store
        .execute(
            shellboy::protocol::Request::new(
                "keyword-delete:live",
                shellboy::protocol::Command::ThreadDelete(shellboy::protocol::ThreadDeleteArgs {
                    thread_id: "live".into(),
                    descendants: false,
                }),
            ),
            None,
        )
        .await
        .unwrap();
    let hidden = search(root, "history", "visibility_needle", SearchMode::Keyword).await;
    assert_eq!(hidden["result"]["items"], json!([]));
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_semantic_accepts_custom_split_and_legacy_without_default_embedding_configuration()
{
    use sha2::{Digest, Sha256};
    use shellboy::{config::Config, index::IndexService, storage::DbWorker};
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let (requests, _, server) = proxy(root).await;
    let original = std::fs::read_to_string(root.join("config.toml")).unwrap();
    let custom = format!("{original}\n[indexing]\nsplit_chars=8\n");
    std::fs::write(root.join("config.toml"), &custom).unwrap();
    let config = Config::parse(&custom).unwrap();
    let endpoint = config.connections["fixture"].base_url.clone();
    std::fs::create_dir(root.join("data")).unwrap();
    let db = DbWorker::open(root.join("data/shellboy.db")).await.unwrap();
    db.call(|db| {
        let payload=json!({"role":"user","content":[{"type":"input_text","text":"NEAR first NEAR second"}]}).to_string();
        let hash=Sha256::digest(payload.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>();
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES('custom','user','now',?1,?2,?3)",rusqlite::params![payload,payload.len()as i64,hash])?;Ok(())
    }).await.unwrap();
    let index = IndexService::new(db.clone(), Arc::new(config));
    index.enqueue_new(&["custom".into()]).await.unwrap();
    while index
        .process_one(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap()
    {}
    let indexed = requests.load(Ordering::SeqCst);
    assert!(
        indexed > 1,
        "fixture actually uses non-default segmentation"
    );
    let read_config = custom.replace("[embedding]\nconnection='fixture'\nmodel='synthetic'\n", "");
    assert!(
        Config::parse(&read_config)
            .unwrap()
            .embedding
            .connection
            .is_none()
    );
    std::fs::write(root.join("config.toml"), &read_config).unwrap();
    let current = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(current["ok"], true, "{current}");
    assert!(!current["result"]["items"].as_array().unwrap().is_empty());
    let changed_split = read_config.replace("split_chars=8", "split_chars=12");
    std::fs::write(root.join("config.toml"), &changed_split).unwrap();
    let changed = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(
        changed["ok"], true,
        "query embedding identity is independent of segmentation: {changed}"
    );
    assert!(!changed["result"]["items"].as_array().unwrap().is_empty());
    let query_count = requests.load(Ordering::SeqCst);
    std::fs::write(
        root.join("config.toml"),
        changed_split.replace(&endpoint, "http://127.0.0.1:1/v1"),
    )
    .unwrap();
    let wrong_endpoint = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(
        wrong_endpoint["result"]["scope_errors"][0]["code"],
        "index_incompatible"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        query_count,
        "reject mismatching endpoint before HTTP"
    );
    std::fs::write(root.join("config.toml"), &changed_split).unwrap();
    db.call(|db| {
        db.execute("DELETE FROM app_meta WHERE key='index.query_profile.'||(SELECT value FROM app_meta WHERE key='index.active_space')", [])?;
        Ok(())
    }).await.unwrap();
    let unverified = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(
        unverified["result"]["scope_errors"][0]["code"], "index_incompatible",
        "do not guess the original split policy: {unverified}"
    );
    assert_eq!(requests.load(Ordering::SeqCst), query_count);
    std::fs::write(root.join("config.toml"), &read_config).unwrap();
    let matching = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(
        matching["ok"], true,
        "unmarked spaces retain strict verified fallback: {matching}"
    );
    assert_eq!(
        db.call(|db| Ok(db.query_row(
            "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'index.query_profile.%'",
            [],
            |row| row.get::<_, i64>(0)
        )?))
        .await
        .unwrap(),
        0,
        "standalone RAG must not supplement owner metadata"
    );
    std::fs::write(root.join("config.toml"), &changed_split).unwrap();
    let legacy =
        Sha256::digest(format!("fixture\0{endpoint}\0synthetic\0unicode-paragraph-v1").as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
    db.call(move|db|{db.execute("UPDATE index_space SET splitter_version=?1 WHERE id=(SELECT value FROM app_meta WHERE key='index.active_space')",[format!("unicode-paragraph-v1:{legacy}")])?;Ok(())}).await.unwrap();
    let old = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(old["ok"], true, "{old}");
    assert!(!old["result"]["items"].as_array().unwrap().is_empty());
    assert_eq!(
        requests.load(Ordering::SeqCst),
        indexed + 4,
        "only explicit query embeddings; no source reindex"
    );
    std::fs::write(
        root.join("config.toml"),
        changed_split.replace(&endpoint, "http://127.0.0.1:1/v1"),
    )
    .unwrap();
    let incompatible = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(
        incompatible["result"]["scope_errors"][0]["code"],
        "index_incompatible"
    );
    assert_eq!(requests.load(Ordering::SeqCst), indexed + 4);
    db.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn history_keyword_streams_large_raw_and_keeps_bounded_witnesses_read_only() {
    use base64::Engine;
    use shellboy::storage::DbWorker;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (requests, _, server) = proxy(root).await;
    std::fs::create_dir(root.join("data")).unwrap();
    let db = DbWorker::open(root.join("data/shellboy.db")).await.unwrap();
    db.call(|c| {
        c.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/tmp','/tmp/AGENTS.md','fixture','model','now');
        INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');
        INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','t','r','completed','now');
        INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','turn',0,0,'fixture','model',0,'completed','{}','now');
        INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','turn','completed','now');")?;
        for (id,ordinal,encoding,canonical) in [("large",0,"UTF-8","CANONICAL"),("bad",1,"not-an-encoding","UNRELATED_CANONICAL")] {
            let payload=json!({"item":{"type":"function_call_output","call_id":id,"output":canonical}}).to_string();
            c.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'tool_result','now',?2,?3)",rusqlite::params![id,payload,payload.len() as i64])?;
            c.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('g',?1,?2,'shell','{}','completed',?1)",rusqlite::params![id,ordinal])?;
            c.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g',?1,?1,'completed')",[id])?;
            c.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at)VALUES(?1,'r','turn',?1,'exited','/tmp','{}','UTF-8',?2,'closed',1,0,1,'now','now')",rusqlite::params![id,encoding])?;
        }
        let mut bytes = vec![b'x';17*1024*1024];
        // Cross both the 64KiB byte decoder page and 64K-character search window.
        let crossing = "XYあ境界MATCH".as_bytes();
        bytes[65534..65534+crossing.len()].copy_from_slice(crossing);
        bytes.extend_from_slice(b" TAIL_MATCH");
        c.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('large',0,'stdout',0,?1,'now')",[bytes])?;
        c.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('bad',0,'stdout',0,x'FF','now')",[])?;
        Ok(())
    }).await.unwrap();
    for query in ["XYあ境界MATCH", "TAIL_MATCH", "UNRELATED_CANONICAL"] {
        let result = search(root, "history", query, SearchMode::Keyword).await;
        assert_eq!(result["ok"], true, "{result}");
        let hit = &result["result"]["items"][0];
        let reference = hit["reference"].as_str().unwrap().to_owned();
        let mut cursor = None;
        let mut body = String::new();
        loop {
            let got = call(
                root,
                RagCommand::Get {
                    reference: reference.clone(),
                    limit_bytes: 65536,
                    cursor,
                },
            )
            .await;
            assert_eq!(got["ok"], true, "{got}");
            body.push_str(got["result"]["text"].as_str().unwrap());
            cursor = got["result"]["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        assert!(
            body.contains(query),
            "bounded reference must retrieve its match"
        );
        assert!(body.chars().count() <= 65536);
        if query == "TAIL_MATCH" {
            let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&reference)
                .unwrap();
            let mut witness: Value = serde_json::from_slice(&raw).unwrap();
            assert!(
                witness["witness"]["raw"]["range"]["start"]
                    .as_u64()
                    .unwrap()
                    > 16 * 1024 * 1024
            );
            witness["witness"]["raw"]
                .as_object_mut()
                .unwrap()
                .remove("range");
            let legacy = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&witness).unwrap());
            assert_eq!(
                call(
                    root,
                    RagCommand::Get {
                        reference: legacy,
                        limit_bytes: 100,
                        cursor: None
                    }
                )
                .await["error"]["code"],
                "input_limit"
            );
            db.call(|c| {
                c.execute(
                    "UPDATE shell_execution SET output_encoding='windows-1252' WHERE id='large'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
            assert_eq!(
                call(
                    root,
                    RagCommand::Get {
                        reference: reference.clone(),
                        limit_bytes: 100,
                        cursor: None
                    }
                )
                .await["error"]["code"],
                "revision_unavailable"
            );
            db.call(|c| {
                c.execute(
                    "UPDATE shell_execution SET output_encoding='UTF-8' WHERE id='large'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
            db.call(|c|{c.execute("UPDATE shell_execution SET retention_generation=retention_generation+1 WHERE id='large'",[])?;Ok(())}).await.unwrap();
            assert_eq!(
                call(
                    root,
                    RagCommand::Get {
                        reference,
                        limit_bytes: 100,
                        cursor: None
                    }
                )
                .await["error"]["code"],
                "revision_unavailable"
            );
        }
    }
    assert_eq!(
        search(root, "history", "", SearchMode::Keyword).await["error"]["code"],
        "invalid_argument"
    );
    assert_eq!(
        search(root, "history", "   ", SearchMode::Keyword).await["error"]["code"],
        "invalid_argument"
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .env_clear()
        .current_dir(root)
        .arg("--data-dir")
        .arg(root.join("data"))
        .arg("--config")
        .arg(root.join("config.toml"))
        .args([
            "rag",
            "search",
            "--scope",
            "history",
            "--query",
            "TAIL_MATCH",
            "--mode",
            "keyword",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["result"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(!root.join("knowledge.db").exists());
    assert!(!root.join("data/rag.db").exists());
    db.call(|c| {
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM output_chunk", [], |r| r
                .get::<_, i64>(0))?,
            2
        );
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM embedding_job", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn history_keyword_crosses_finite_id_batches_without_creating_an_index() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir(root.join("data")).unwrap();
    let db = shellboy::storage::DbWorker::open(root.join("data/shellboy.db"))
        .await
        .unwrap();
    db.call(|c|{
        for ordinal in 0..65 {
            let text=if ordinal==64 {"LAST_BATCH_MATCH"} else {"unrelated"};
            let payload=json!({"role":"user","content":[{"type":"input_text","text":text}]}).to_string();
            c.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'user','now',?2,?3)",rusqlite::params![format!("node-{ordinal}"),payload,payload.len() as i64])?;
        }
        Ok(())
    }).await.unwrap();
    let result = search(root, "history", "LAST_BATCH_MATCH", SearchMode::Keyword).await;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        result["result"]["items"][0]["location"]["node_id"],
        "node-64"
    );
    db.call(|c| {
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM node", [], |r| r.get::<_, i64>(0))?,
            65
        );
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM embedding_job", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM index_space", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    assert!(!root.join("knowledge.db").exists());
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn docs_semantic_requires_explicit_index_and_search_never_prepares_guides() {
    use fs2::FileExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (requests, fail_at, server) = proxy(root).await;
    let missing = search(root, "docs", "recovery", SearchMode::Semantic).await;
    assert_eq!(
        missing["result"]["scope_errors"][0]["code"], "db_not_found",
        "{missing}"
    );
    let default = call(
        root,
        RagCommand::Search {
            query: "recovery".into(),
            scope: vec![],
            mode: SearchMode::Hybrid,
            limit: 10,
        },
    )
    .await;
    assert_eq!(default["ok"], true);
    assert_eq!(default["result"]["degraded"], true);
    assert_eq!(default["result"]["scopes"], json!(["docs"]));
    assert!(!default["result"]["items"].as_array().unwrap().is_empty());
    assert!(!root.join("knowledge.db").exists());
    assert!(!root.join("data").exists());
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    init(root).await;
    let preview = call(
        root,
        RagCommand::Index {
            source: Some("docs".into()),
            dry_run: true,
            mode: IndexMode::Semantic,
        },
    )
    .await;
    assert_eq!(preview["ok"], true, "{preview}");
    assert!(preview["result"]["documents"].as_u64().unwrap() > 0);
    assert!(preview["result"]["text_bytes"].as_u64().unwrap() > 0);
    assert!(preview["result"]["segments"].as_u64().unwrap() > 0);
    assert_eq!(preview["result"]["embedding_may_charge"], true);
    assert_eq!(index(root, None, IndexMode::Semantic).await["ok"], true);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "default index does not opt into bundled guides"
    );
    assert_eq!(
        call(root, RagCommand::Status).await["result"]["documents"],
        0
    );
    let not_ready = search(root, "docs", "recovery", SearchMode::Semantic).await;
    assert_eq!(
        not_ready["result"]["scope_errors"][0]["code"],
        "index_incompatible"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    fail_at.store(2, Ordering::SeqCst);
    let failed = index(root, Some("docs"), IndexMode::Semantic).await;
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    let db = rusqlite::Connection::open(root.join("knowledge.db")).unwrap();
    let cached: i64 = db
        .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
        .unwrap();
    assert!(cached > 0, "successful batches survive the later failure");
    assert_eq!(
        search(root, "docs", "recovery", SearchMode::Semantic).await["ok"],
        false
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "search cannot retry document embeddings"
    );
    fail_at.store(usize::MAX, Ordering::SeqCst);
    assert_eq!(
        index(root, Some("docs"), IndexMode::Semantic).await["ok"],
        true
    );
    let indexed = requests.load(Ordering::SeqCst);
    assert_eq!(
        index(root, Some("@docs"), IndexMode::Semantic).await["ok"],
        true
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        indexed,
        "existing and legacy-format successful cache is reused"
    );
    let generation: String = db
        .query_row("SELECT value FROM meta WHERE key='generation'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("knowledge.rag-lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    let found = search(root, "docs", "recovery", SearchMode::Semantic).await;
    assert_eq!(found["ok"], true, "{found}");
    assert!(!found["result"]["items"].as_array().unwrap().is_empty());
    assert_eq!(
        requests.load(Ordering::SeqCst),
        indexed + 1,
        "only explicit query embedding is billed"
    );
    assert_eq!(
        db.query_row("SELECT value FROM meta WHERE key='generation'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        generation
    );
    let reference = found["result"]["items"][0]["reference"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        call(
            root,
            RagCommand::Get {
                reference,
                limit_bytes: 100,
                cursor: None
            }
        )
        .await["ok"],
        true
    );
    drop(lock);
    let original_config = std::fs::read_to_string(root.join("config.toml")).unwrap();
    std::fs::write(
        root.join("config.toml"),
        original_config.replace("synthetic", "other-profile-model"),
    )
    .unwrap();
    let profile_missing = search(root, "docs", "recovery", SearchMode::Semantic).await;
    assert_eq!(
        profile_missing["result"]["scope_errors"][0]["code"],
        "index_incompatible"
    );
    assert!(
        profile_missing["result"]["scope_errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("--source docs")
    );
    assert_eq!(requests.load(Ordering::SeqCst), indexed + 1);
    std::fs::write(root.join("config.toml"), original_config).unwrap();
    // Simulate a binary whose catalogue added a guide: no current published revision.
    db.execute("UPDATE documents SET active_revision=NULL WHERE source_id='@docs' AND key=(SELECT key FROM documents WHERE source_id='@docs' LIMIT 1)",[]).unwrap();
    let unavailable = search(root, "docs", "recovery", SearchMode::Hybrid).await;
    assert_eq!(unavailable["ok"], true);
    assert_eq!(unavailable["result"]["degraded"], true);
    assert_eq!(unavailable["result"]["scope_errors"][0]["warning"], true);
    assert!(
        !unavailable["result"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(requests.load(Ordering::SeqCst), indexed + 1);
    assert_eq!(
        index(root, Some("docs"), IndexMode::Semantic).await["ok"],
        true
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        indexed + 1,
        "restoring publication reuses saved embeddings"
    );
    // An obsolete guide is removed only by explicit synchronization, never by search.
    db.execute(
        "INSERT INTO documents(id,source_id,key)VALUES('obsolete','@docs','removed-guide')",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO revisions SELECT 'obsolete-revision','obsolete',source_hash,body_hash,extractor,kind,title,provenance,created_at FROM revisions LIMIT 1",[]).unwrap();
    db.execute(
        "UPDATE documents SET active_revision='obsolete-revision' WHERE id='obsolete'",
        [],
    )
    .unwrap();
    assert_eq!(
        search(root, "docs", "recovery", SearchMode::Semantic).await["ok"],
        false
    );
    assert_eq!(requests.load(Ordering::SeqCst), indexed + 1);
    assert!(
        db.query_row(
            "SELECT active_revision IS NOT NULL FROM documents WHERE id='obsolete'",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
    assert_eq!(
        index(root, Some("docs"), IndexMode::Semantic).await["result"]["removed"],
        1
    );
    assert_eq!(requests.load(Ordering::SeqCst), indexed + 1);
    assert!(
        !db.query_row(
            "SELECT active_revision IS NOT NULL FROM documents WHERE id='obsolete'",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
    assert_eq!(
        search(root, "", "recovery", SearchMode::Keyword).await["ok"],
        false
    );
    let defaults = call(
        root,
        RagCommand::Search {
            query: "recovery".into(),
            scope: vec![],
            mode: SearchMode::Semantic,
            limit: 10,
        },
    )
    .await;
    assert_eq!(defaults["ok"], true, "{defaults}");
    assert_eq!(
        defaults["result"]["scopes"],
        json!(["docs"]),
        "unpopulated automatic derived container is not registered material"
    );
    assert_eq!(requests.load(Ordering::SeqCst), indexed + 2);
    std::fs::write(root.join("notes.md"), "復旧の一次資料").unwrap();
    add(root, "notes", "notes.md").await;
    assert_eq!(
        index(root, Some("notes"), IndexMode::Semantic).await["ok"],
        true
    );
    let before_default = requests.load(Ordering::SeqCst);
    let with_materials = call(
        root,
        RagCommand::Search {
            query: "recovery".into(),
            scope: vec![],
            mode: SearchMode::Semantic,
            limit: 10,
        },
    )
    .await;
    assert_eq!(with_materials["ok"], true, "{with_materials}");
    assert_eq!(
        with_materials["result"]["scopes"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        before_default + 2,
        "docs and registered materials query embeddings only"
    );
    let ingested = call(
        root,
        RagCommand::Ingest {
            id: "explanation".into(),
            source_file: root.join("notes.md"),
            kind: shellboy::rag::IngestKind::LlmExplanation,
            text_file: Some(root.join("notes.md")),
            text_stdin: false,
            metadata_file: None,
        },
    )
    .await;
    assert_eq!(ingested["ok"], true, "{ingested}");
    let before_keyword = requests.load(Ordering::SeqCst);
    let all = call(
        root,
        RagCommand::Search {
            query: "recovery".into(),
            scope: vec![],
            mode: SearchMode::Keyword,
            limit: 10,
        },
    )
    .await;
    assert_eq!(all["ok"], true, "{all}");
    assert!(
        all["result"]["scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("source:@derived")),
        "populated derived material remains in default search"
    );
    assert_eq!(requests.load(Ordering::SeqCst), before_keyword);
    server.abort();
}

#[tokio::test]
async fn successful_semantic_scope_breaks_provider_failure_streak_without_extra_authorization() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (requests, fail_at, server) = proxy(root).await;
    init(root).await;
    let mut scopes = Vec::new();
    for i in 0..7 {
        let name = format!("scope{i}");
        std::fs::create_dir(root.join(&name)).unwrap();
        std::fs::write(
            root.join(&name).join("note.md"),
            format!("unique scope {i}"),
        )
        .unwrap();
        add(root, &name, &name).await;
        scopes.push(format!("source:{name}"));
    }
    assert_eq!(index(root, None, IndexMode::Semantic).await["ok"], true);
    requests.store(0, Ordering::SeqCst);
    fail_at.store(usize::MAX - 2, Ordering::SeqCst);
    let result = call(
        root,
        RagCommand::Search {
            query: "explicit bounded query".into(),
            scope: scopes.clone(),
            mode: SearchMode::Semantic,
            limit: 10,
        },
    )
    .await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        7,
        "intervening successful scopes must break the streak: {result}"
    );
    let errors = result["result"]["scope_errors"].as_array().unwrap();
    assert_eq!(errors.len(), 4);
    assert!(errors.iter().all(|e| e["code"] == "embedding_failed"));
    assert_eq!(result["result"]["items"].as_array().unwrap().len(), 3);
    requests.store(0, Ordering::SeqCst);
    fail_at.store(usize::MAX - 3, Ordering::SeqCst);
    let stopped = call(
        root,
        RagCommand::Search {
            query: "another explicit query".into(),
            scope: scopes,
            mode: SearchMode::Semantic,
            limit: 10,
        },
    )
    .await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        3,
        "three consecutive failures stop later scopes: {stopped}"
    );
    let errors = stopped["result"]["scope_errors"].as_array().unwrap();
    assert_eq!(
        errors
            .iter()
            .filter(|e| e["code"] == "embedding_paused")
            .count(),
        4
    );
    server.abort();
}

#[tokio::test]
async fn successful_derived_revision_breaks_provider_failure_streak_and_retry_reuses_cache() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (requests, fail_at, server) = proxy(root).await;
    init(root).await;
    for i in 0..7 {
        let text = root.join(format!("explanation{i}.md"));
        std::fs::write(&text, format!("unique derived body {i}")).unwrap();
        let ingested = call(
            root,
            RagCommand::Ingest {
                id: format!("explanation{i}"),
                source_file: text.clone(),
                kind: shellboy::rag::IngestKind::LlmExplanation,
                text_file: Some(text),
                text_stdin: false,
                metadata_file: None,
            },
        )
        .await;
        assert_eq!(ingested["ok"], true, "{ingested}");
    }
    fail_at.store(usize::MAX - 2, Ordering::SeqCst);
    let partial = index(root, Some("derived"), IndexMode::Semantic).await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        7,
        "successful derived revisions must break the streak: {partial}"
    );
    assert_eq!(partial["result"]["failure_count"], 4);
    assert_eq!(partial["result"]["deferred"], 0);
    assert_eq!(partial["result"]["embedding_paused"], false);
    fail_at.store(usize::MAX, Ordering::SeqCst);
    assert_eq!(
        index(root, Some("derived"), IndexMode::Semantic).await["ok"],
        true
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        11,
        "retry only the four failed authorized revisions"
    );
    assert_eq!(
        index(root, Some("derived"), IndexMode::Semantic).await["ok"],
        true
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        11,
        "all successful batches remain cached"
    );
    server.abort();
}

#[tokio::test]
async fn history_semantic_streams_large_raw_spans_and_preserves_valid_hits() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    use shellboy::{config::Config, index::IndexService, storage::DbWorker};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (requests, _, server) = proxy(root).await;
    std::fs::create_dir(root.join("data")).unwrap();
    let db = DbWorker::open(root.join("data/shellboy.db")).await.unwrap();
    db.call(|c| {
        c.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/tmp','/tmp/AGENTS.md','fixture','model','now');
        INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');
        INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','t','r','completed','now');
        INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','turn',0,0,'fixture','model',0,'completed','{}','now');
        INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','turn','completed','now');
        INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('g','large',0,'shell','{}','completed','large');
        INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at)VALUES('large','r','turn','large','exited','/tmp','{}','UTF-8','UTF-8','closed',1,0,1,'now','now');")?;
        for (id, kind, payload) in [
            ("large", "tool_result", json!({"item":{"type":"function_call_output","call_id":"large","output":"NEAR canonical tool result"}})),
            ("normal", "user", json!({"role":"user","content":[{"type":"input_text","text":"NEAR ordinary history"}]})),
        ] {
            let payload = payload.to_string();
            let hash = Sha256::digest(payload.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>();
            c.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,?2,'now',?3,?4,?5)",rusqlite::params![id,kind,payload,payload.len() as i64,hash])?;
        }
        c.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','large','large','completed')", [])?;
        c.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('large',0,'stdout',0,?1,'now')",["NEAR あ境界".as_bytes()])?;
        Ok(())
    }).await.unwrap();
    let config =
        Config::parse(&std::fs::read_to_string(root.join("config.toml")).unwrap()).unwrap();
    let service = IndexService::new(db.clone(), Arc::new(config));
    service
        .enqueue_new(&["large".into(), "normal".into()])
        .await
        .unwrap();
    while service
        .process_one(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap()
    {}
    let small = search(root, "history", "NEAR", SearchMode::Hybrid).await;
    assert_eq!(small["ok"], true, "{small}");
    let raw = small["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["location"]["body_kind"] == "tool_raw")
        .unwrap();
    assert!(
        raw["location"]["range"].is_null(),
        "small full-body references remain compatible with keyword fusion"
    );
    let reference = raw["reference"].as_str().unwrap();
    let got = call(
        root,
        RagCommand::Get {
            reference: reference.into(),
            limit_bytes: 65536,
            cursor: None,
        },
    )
    .await;
    assert_eq!(got["result"]["text"], "NEAR あ境界");

    // Seed a completed, valid tail span directly: this is the same saved index
    // representation as backfill, without thousands of irrelevant HTTP calls.
    const PREFIX: usize = 17 * 1024 * 1024;
    db.call(|c| {
        let mut bytes = vec![b'x'; PREFIX];
        bytes.extend_from_slice("NEAR あ境界".as_bytes());
        c.execute("UPDATE output_chunk SET bytes=?1 WHERE execution_id='large'",[bytes])?;
        c.execute("UPDATE embedding_job SET span_start=span_start+?1,span_end=span_end+?1 WHERE body_kind='tool_raw'",[PREFIX as i64])?;
        c.execute("UPDATE embedding_chunk SET span_start=span_start+?1,span_end=span_end+?1 WHERE job_id IN(SELECT id FROM embedding_job WHERE body_kind='tool_raw')",[PREFIX as i64])?;
        Ok(())
    }).await.unwrap();
    let before_queries = requests.load(Ordering::SeqCst);
    let found = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(found["ok"], true, "{found}");
    let items = found["result"]["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|hit| hit["location"]["node_id"] == "normal")
    );
    let raw = items
        .iter()
        .find(|hit| hit["location"]["body_kind"] == "tool_raw")
        .unwrap();
    assert_eq!(raw["location"]["range"]["start"], PREFIX);
    assert_eq!(raw["excerpt"], "NEAR あ境界");
    let reference = raw["reference"].as_str().unwrap().to_owned();
    let got = call(
        root,
        RagCommand::Get {
            reference: reference.clone(),
            limit_bytes: 65536,
            cursor: None,
        },
    )
    .await;
    assert_eq!(got["ok"], true, "{got}");
    assert_eq!(got["result"]["text"], "NEAR あ境界");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        before_queries + 1,
        "only query embedding, no source reindex/get embedding"
    );
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&reference)
        .unwrap();
    let witness: Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(witness["witness"]["raw"]["range"], raw["location"]["range"]);

    // A legal splitter can produce spans larger than the retrieval window.
    // Hash the complete span, but retain and page only its first 64K characters.
    db.call(|c| {
        let mut bytes = vec![b'x'; PREFIX];
        bytes.extend_from_slice("NEAR あ境界".as_bytes());
        let hash = Sha256::digest(&bytes).iter().map(|b|format!("{b:02x}")).collect::<String>();
        let end = PREFIX + "NEAR あ境界".chars().count();
        c.execute("UPDATE embedding_job SET span_start=0,span_end=?1,source_hash=?2 WHERE body_kind='tool_raw'",rusqlite::params![end as i64,hash])?;
        c.execute("UPDATE embedding_chunk SET span_start=0,span_end=?1,content_hash=?2 WHERE job_id IN(SELECT id FROM embedding_job WHERE body_kind='tool_raw')",rusqlite::params![end as i64,hash])?;
        Ok(())
    }).await.unwrap();
    let wide = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(wide["ok"], true, "{wide}");
    let raw = wide["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["location"]["body_kind"] == "tool_raw")
        .unwrap();
    assert_eq!(raw["location"]["range"], json!({"start":0,"end":65536}));
    assert!(raw["location"]["indexed_range"]["end"].as_u64().unwrap() > 16 * 1024 * 1024);
    let wide_reference = raw["reference"].as_str().unwrap().to_owned();
    let got = call(
        root,
        RagCommand::Get {
            reference: wide_reference.clone(),
            limit_bytes: 65536,
            cursor: None,
        },
    )
    .await;
    assert_eq!(got["result"]["text"].as_str().unwrap().len(), 65536);

    // Corruption outside the returned prefix still invalidates the indexed
    // span. It must not become an apparently successful empty/partial hit.
    db.call(|c| {
        let mut bytes = vec![b'x'; PREFIX];
        bytes.extend_from_slice("NEAR い境界".as_bytes());
        c.execute(
            "UPDATE output_chunk SET bytes=?1 WHERE execution_id='large'",
            [bytes],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let corrupt = search(root, "history", "NEAR", SearchMode::Semantic).await;
    assert_eq!(corrupt["ok"], false, "{corrupt}");
    assert_eq!(
        corrupt["result"]["scope_errors"][0]["code"],
        "revision_unavailable"
    );
    let stale = call(
        root,
        RagCommand::Get {
            reference,
            limit_bytes: 65536,
            cursor: None,
        },
    )
    .await;
    assert_eq!(stale["error"]["code"], "revision_unavailable");
    db.call(|c| { c.execute("UPDATE shell_execution SET retention_generation=retention_generation+1 WHERE id='large'", [])?; Ok(()) }).await.unwrap();
    let expired = call(
        root,
        RagCommand::Get {
            reference: wide_reference,
            limit_bytes: 65536,
            cursor: None,
        },
    )
    .await;
    assert_eq!(expired["error"]["code"], "revision_unavailable");
    db.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn large_original_keeps_observed_and_claimed_hashes_and_survives_original_removal() {
    use sha2::{Digest, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    init(root).await;
    let original = root.join("original.pdf");
    std::fs::File::create(&original)
        .unwrap()
        .set_len(17 * 1024 * 1024)
        .unwrap();
    let expected = Sha256::digest(std::fs::read(&original).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    std::fs::write(
        root.join("summary.md"),
        "SYNTHETIC_DERIVED_SUMMARY 日本語の外部解釈",
    )
    .unwrap();
    std::fs::write(root.join("metadata.json"), json!({"model":"fixture-model","generated_at":"2026-10-28T00:00:00Z","source_format":"application/pdf","source_hash":"generation-input-claim","review_required":true}).to_string()).unwrap();
    let ingested = call(
        root,
        RagCommand::Ingest {
            id: "external-summary".into(),
            source_file: original.clone(),
            kind: shellboy::rag::IngestKind::LlmExplanation,
            text_file: Some(root.join("summary.md")),
            text_stdin: false,
            metadata_file: Some(root.join("metadata.json")),
        },
    )
    .await;
    assert_eq!(ingested["ok"], true, "{ingested}");
    std::fs::remove_file(original).unwrap();
    let found = search(
        root,
        "source:derived",
        "SYNTHETIC_DERIVED_SUMMARY",
        SearchMode::Keyword,
    )
    .await;
    assert_eq!(found["ok"], true, "{found}");
    let item = &found["result"]["items"][0];
    assert_eq!(item["kind"], "llm-explanation");
    let source = &item["provenance"]["source"];
    assert_eq!(
        source["original_path"],
        root.join("original.pdf").to_string_lossy().as_ref()
    );
    assert_eq!(source["observed_original_hash"], expected);
    assert_eq!(source["original_hash_claim"], "generation-input-claim");
    assert_eq!(source["metadata"]["review_required"], true);
    assert_ne!(
        source["derived_body_hash"],
        source["observed_original_hash"]
    );
    let fetched = call(
        root,
        RagCommand::Get {
            reference: item["reference"].as_str().unwrap().into(),
            limit_bytes: 65536,
            cursor: None,
        },
    )
    .await;
    assert_eq!(fetched["ok"], true, "{fetched}");
    assert!(
        fetched
            .to_string()
            .contains("at_ingest_not_proof_of_generation_input")
    );
    assert!(fetched.to_string().contains("SYNTHETIC_DERIVED_SUMMARY"));
}

#[tokio::test]
async fn previously_extracted_office_revision_becomes_removed_without_a_failure() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir(root.join("materials")).unwrap();
    std::fs::write(root.join("materials/legacy.md"), "OLD_OFFICE_DERIVED_TEXT").unwrap();
    init(root).await;
    add(root, "notes", "materials").await;
    assert_eq!(
        index(root, Some("notes"), IndexMode::Keyword).await["ok"],
        true
    );
    // Represent a prior native PDF extraction in the same persisted schema.
    let db = rusqlite::Connection::open(root.join("knowledge.db")).unwrap();
    db.execute(
        "UPDATE documents SET key='legacy.pdf' WHERE key='legacy.md'",
        [],
    )
    .unwrap();
    db.execute("UPDATE revisions SET extractor='xberg:1.3.10'", [])
        .unwrap();
    std::fs::rename(
        root.join("materials/legacy.md"),
        root.join("materials/legacy.pdf"),
    )
    .unwrap();
    let result = index(root, Some("notes"), IndexMode::Keyword).await;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["removed"], 1);
    assert_eq!(result["result"]["failure_count"], 0);
    assert_eq!(
        result["result"]["sources"][0]["selection"]["unsupported"],
        1
    );
    assert_eq!(
        search(
            root,
            "source:notes",
            "OLD_OFFICE_DERIVED_TEXT",
            SearchMode::Keyword
        )
        .await["result"]["items"],
        json!([])
    );
}
