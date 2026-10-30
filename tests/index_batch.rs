//! Deterministic batching against real sqlite-vec and synthetic loopback HTTP.
use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shellboy::{
    config::{Config, Connection},
    index::IndexService,
    protocol::IndexArgs,
    storage::DbWorker,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Replies {
    reject_multi: bool,
    invalid_multi: bool,
    auth: bool,
    split_word: Option<String>,
}
struct Proxy {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    replies: Arc<Mutex<Replies>>,
    stop: CancellationToken,
    task: JoinHandle<()>,
}
impl Proxy {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let replies = Arc::new(Mutex::new(Replies::default()));
        let response = replies.clone();
        let stop = CancellationToken::new();
        let cancel = stop.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! { _=cancel.cancelled()=>return, value=listener.accept()=>value.unwrap() };
                let (mut socket, _) = accepted;
                let mut bytes = Vec::new();
                let request = loop {
                    let mut chunk = [0; 8192];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "fixture request closed early");
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let length = String::from_utf8_lossy(&bytes[..at])
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= at + 4 + length {
                            break serde_json::from_slice::<Value>(&bytes[at + 4..at + 4 + length])
                                .unwrap();
                        }
                    }
                };
                captured.lock().unwrap().push(request.clone());
                let inputs = request["input"].as_array().unwrap();
                let (status, body) = {
                    let rules = response.lock().unwrap();
                    if rules.auth {
                        (
                            401,
                            json!({"error":{"code":"invalid_api_key","message":"synthetic authorization failure"}}),
                        )
                    } else if inputs.len() > 1 && rules.reject_multi {
                        (
                            400,
                            json!({"error":{"code":"unsupported_batch","message":"synthetic multi-input rejection"}}),
                        )
                    } else if inputs.len() == 1 && rules.split_word.as_deref() == inputs[0].as_str()
                    {
                        (
                            400,
                            json!({"error":{"code":"input_too_long","message":"synthetic individual length failure"}}),
                        )
                    } else {
                        // Valid responses are intentionally reversed: indices,
                        // not array position, must determine vector ownership.
                        let data=inputs.iter().enumerate().rev().map(|(index,input)| {
                            let text=input.as_str().unwrap();
                            json!({"index":if rules.invalid_multi&&inputs.len()>1 {0}else{index},"embedding":[text.chars().next().unwrap_or(' ')as u32 as f32,text.chars().count()as f32]})
                        }).collect::<Vec<_>>();
                        (200, json!({"data":data}))
                    }
                };
                let body = body.to_string();
                socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        Self {
            url,
            requests,
            replies,
            stop,
            task,
        }
    }
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
    async fn close(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
}
struct Fixture {
    root: tempfile::TempDir,
    path: PathBuf,
    db: DbWorker,
    service: Arc<IndexService>,
    config: Arc<Config>,
    proxy: Proxy,
}
impl Fixture {
    async fn new(max_inputs: usize, max_chars: usize, split_chars: usize) -> Self {
        let proxy = Proxy::new().await;
        let mut config = Config {
            connections: BTreeMap::from([(
                "fixture".into(),
                Connection {
                    base_url: proxy.url.clone(),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        config.embedding.connection = Some("fixture".into());
        config.embedding.model = Some("synthetic".into());
        config.embedding.batch_max_inputs = max_inputs;
        config.embedding.batch_max_chars = max_chars;
        config.indexing.split_chars = split_chars;
        config.indexing.retries = 0;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.db");
        let db = DbWorker::open(&path).await.unwrap();
        let config = Arc::new(config);
        let service = IndexService::new(db.clone(), config.clone());
        let value = Self {
            root,
            path,
            db,
            service,
            config,
            proxy,
        };
        value.nodes(&[("seed", "猫")]).await;
        assert!(
            value
                .service
                .process_one(CancellationToken::new())
                .await
                .unwrap()
        );
        value.drain().await;
        assert_eq!(
            value.proxy.requests().len(),
            1,
            "one real bootstrap, no extra probe"
        );
        value
    }
    async fn nodes(&self, nodes: &[(&str, &str)]) {
        let rows = nodes
            .iter()
            .map(|(id, text)| (id.to_string(), text.to_string()))
            .collect::<Vec<_>>();
        let ids = rows.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        self.db.call(move |db| {
            let tx=db.transaction()?;
            for (id,text) in rows {
                let body=json!({"role":"user","content":[{"type":"input_text","text":text}]}).to_string();
                let hash=Sha256::digest(body.as_bytes()).iter().map(|byte|format!("{byte:02x}")).collect::<String>();
                tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,'user','2026-01-01T00:00:00Z',?2,?3,?4)",params![id,body,body.len()as i64,hash])?;
            }
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        self.service.enqueue_new(&ids).await.unwrap();
    }
    async fn drain(&self) {
        drain(&self.service).await;
    }
    async fn close(self) {
        self.db.shutdown().await.unwrap();
        self.proxy.close().await;
    }
}
async fn drain(service: &IndexService) {
    for _ in 0..100 {
        if !service.process_one(CancellationToken::new()).await.unwrap() {
            return;
        }
    }
    panic!("bounded fixture did not drain");
}

#[tokio::test]
async fn same_snapshot_has_same_unicode_bounded_batches_and_fewer_requests() {
    let f = Fixture::new(3, 4, 4).await;
    f.nodes(&[
        ("one", "猫a"),
        ("two", "犬b"),
        ("three", "鳥c"),
        ("four", "魚d"),
    ])
    .await;
    f.db.call(|db| {
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    })
    .await
    .unwrap();
    let snapshot = f.root.path().join("snapshot.db");
    std::fs::copy(&f.path, &snapshot).unwrap();
    f.drain().await;
    f.db.call(|db| {
        let table:String=db.query_row("SELECT vec_table_name FROM index_space WHERE state='active'",[],|row|row.get(0))?;
        let rows=db.prepare(&format!("SELECT c.source_node_id,vec_to_json(v.embedding) FROM embedding_chunk c JOIN {table} v ON v.rowid=c.vec_row_id"))?
            .query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (source,encoded) in rows {
            let expected=match source.as_str(){"seed"=>('猫',1),"one"=>('猫',2),"two"=>('犬',2),"three"=>('鳥',2),"four"=>('魚',2),_=>panic!("unexpected source")};
            let vector:Vec<f32>=serde_json::from_str(&encoded).unwrap();
            assert_eq!(vector,vec![expected.0 as u32 as f32,expected.1 as f32],"batch indices must retain source ownership");
        }
        Ok(())
    }).await.unwrap();
    let requests = f.proxy.requests();
    assert_eq!(
        requests.len(),
        3,
        "bootstrap plus two batches, instead of five requests"
    );
    for request in &requests[1..] {
        let input = request["input"].as_array().unwrap();
        assert_eq!(input.len(), 2, "Unicode cap wins over input count cap");
        assert_eq!(
            input
                .iter()
                .map(|value| value.as_str().unwrap().chars().count())
                .sum::<usize>(),
            4
        );
        assert!(request.get("dimensions").is_none());
    }
    let db = DbWorker::open(&snapshot).await.unwrap();
    let service = IndexService::new(db.clone(), f.config.clone());
    drain(&service).await;
    let repeated = f.proxy.requests();
    assert_eq!(
        &repeated[3..],
        &requests[1..],
        "saved IDs/order and settings define the batch"
    );
    let status = service.status(IndexArgs::default()).await.unwrap();
    assert_eq!(status.done, 5);
    db.shutdown().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn count_limit_is_independent_and_default_single_request_shape_is_unchanged() {
    let f = Fixture::new(2, 64, 16).await;
    f.nodes(&[("a", "alpha"), ("b", "beta"), ("c", "gamma")])
        .await;
    f.drain().await;
    let requests = f.proxy.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["input"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        vec![1, 2, 1]
    );
    f.close().await;
    let f = Fixture::new(1, 64, 16).await;
    f.nodes(&[("one", "alpha"), ("two", "beta")]).await;
    f.drain().await;
    for request in f.proxy.requests() {
        assert_eq!(request.as_object().unwrap().len(), 3);
        assert_eq!(request["encoding_format"], "float");
        assert_eq!(request["model"], "synthetic");
        assert_eq!(request["input"].as_array().unwrap().len(), 1);
    }
    f.close().await;
}

#[tokio::test]
async fn rejected_batch_records_every_member_then_retries_singly_even_without_retry_budget() {
    let f = Fixture::new(8, 64, 16).await;
    f.nodes(&[("one", "alpha"), ("two", "beta"), ("three", "gamma")])
        .await;
    f.proxy.replies.lock().unwrap().reject_multi = true;
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    f.db.call(|db| {
        let rows = db
            .prepare("SELECT state,error_json FROM embedding_job WHERE source_node_id<>'seed'")?
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(rows.len(), 3);
        for (state, error) in rows {
            assert_eq!(state, "pending");
            let error: Value = serde_json::from_str(&error).unwrap();
            assert_eq!(error["kind"], "embedding_batch_failure");
            assert_eq!(error["input_count"], 3);
        }
        assert_eq!(
            db.query_row("SELECT count(*) FROM embedding_chunk", [], |row| row
                .get::<_, i64>(0))?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    assert!(!f.service.status(IndexArgs::default()).await.unwrap().paused);
    f.drain().await;
    let requests = f.proxy.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["input"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        vec![1, 3, 1, 1, 1]
    );
    assert_eq!(
        f.service.status(IndexArgs::default()).await.unwrap().done,
        4
    );
    f.close().await;
}

#[tokio::test]
async fn malformed_batch_adopts_nothing_and_individual_length_children_stay_single() {
    let f = Fixture::new(8, 64, 16).await;
    f.nodes(&[("one", "alpha"), ("two", "bad"), ("three", "gamma")])
        .await;
    f.proxy.replies.lock().unwrap().invalid_multi = true;
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    f.db.call(|db| {
        assert_eq!(
            db.query_row("SELECT count(*) FROM embedding_chunk", [], |row| row
                .get::<_, i64>(0))?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    f.proxy.replies.lock().unwrap().split_word = Some("bad".into());
    f.drain().await;
    let requests = f.proxy.requests();
    assert_eq!(requests[1]["input"].as_array().unwrap().len(), 3);
    assert!(
        requests[2..]
            .iter()
            .all(|request| request["input"].as_array().unwrap().len() == 1)
    );
    let status = f.service.status(IndexArgs::default()).await.unwrap();
    assert_eq!(status.failed, 0);
    assert!(!status.paused);
    f.close().await;
}

#[tokio::test]
async fn common_auth_failure_pauses_and_transaction_failure_never_leaves_partial_vectors() {
    let f = Fixture::new(8, 64, 16).await;
    f.nodes(&[("one", "alpha"), ("two", "beta"), ("three", "gamma")])
        .await;
    f.proxy.replies.lock().unwrap().auth = true;
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    let status = f.service.status(IndexArgs::default()).await.unwrap();
    assert!(status.paused);
    assert_eq!(status.failed, 3);
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(f.proxy.requests().len(), 2);
    f.close().await;
    let f = Fixture::new(8, 64, 16).await;
    f.nodes(&[("good", "alpha"), ("bad", "beta")]).await;
    f.db.call(|db| {
        let tx=db.transaction()?;
        // Fix saved IDs so a successful vector/chunk insert happens before the
        // injected second-member failure; the assertion truly needs rollback.
        let rows=tx.prepare("SELECT id,source_node_id FROM embedding_job WHERE source_node_id IN ('good','bad')")?
            .query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (old,source) in rows {
            let id=if source=="good" {"batch-order-0-good"} else {"batch-order-1-bad"};
            tx.execute("UPDATE embedding_job SET id=?2 WHERE id=?1",params![old,id])?;
            tx.execute("UPDATE app_meta SET key=?2 WHERE key=?1",params![format!("index.input.{old}"),format!("index.input.{id}")])?;
        }
        tx.execute_batch("CREATE TRIGGER reject_batch_chunk BEFORE INSERT ON embedding_chunk WHEN NEW.source_node_id='bad' BEGIN SELECT RAISE(ABORT,'synthetic vector adoption failure'); END;")?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    f.db.call(|db| {
        assert_eq!(db.query_row("SELECT count(*) FROM embedding_chunk",[],|row|row.get::<_,i64>(0))?,1);
        let table:String=db.query_row("SELECT vec_table_name FROM index_space WHERE state='active'",[],|row|row.get(0))?;
        assert_eq!(db.query_row(&format!("SELECT count(*) FROM {table}"),[],|row|row.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT count(*) FROM embedding_job WHERE source_node_id<>'seed' AND state='failed'",[],|row|row.get::<_,i64>(0))?,2);
        Ok(())
    }).await.unwrap();
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(
        f.proxy.requests().len(),
        2,
        "failed adoption must not resend HTTP"
    );
    f.close().await;
}

#[tokio::test]
async fn retry_preview_matches_current_targets_and_reports_unread_admissions() {
    let f = Fixture::new(1, 64, 16).await;
    f.nodes(&[("alpha", "aa"), ("beta", "bbb")]).await;
    let active = f.db.call(|db| {
        let active: String = db.query_row("SELECT value FROM app_meta WHERE key='index.active_space'", [], |r| r.get(0))?;
        let splitter: String = db.query_row("SELECT splitter_version FROM index_space WHERE id=?1", [&active], |r| r.get(0))?;
        let profile = splitter.strip_prefix("unicode-paragraph-v1:").unwrap();
        db.execute("UPDATE embedding_job SET state='failed' WHERE source_node_id IN ('alpha','beta')", [])?;
        for (id,state,job) in [("retired", "inactive", "retired-job"), ("candidate", "candidate", "candidate-job")] {
            db.execute("INSERT INTO index_space SELECT ?1,profile_id,model_id,dimension,splitter_version,?2,?3 FROM index_space WHERE id=?4", params![id,state,format!("unused-{id}"),active])?;
            db.execute("INSERT INTO embedding_job SELECT ?1,?2,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,'failed',attempts,error_json,next_attempt_at,authorized_watermark FROM embedding_job WHERE source_node_id='alpha' AND space_id=?3", params![job,id,active])?;
            db.execute("INSERT INTO app_meta SELECT ?1,value FROM app_meta WHERE key=(SELECT 'index.input.'||id FROM embedding_job WHERE source_node_id='alpha' AND space_id=?2)", params![format!("index.input.{job}"),active])?;
        }
        db.execute("INSERT INTO app_meta VALUES(?1,'current')", [format!("index.rebuild.{profile}")])?;
        db.execute("INSERT INTO app_meta VALUES(?1,'candidate')", [format!("index.group.{profile}.current")])?;
        let input: String = db.query_row("SELECT value FROM app_meta WHERE key=(SELECT 'index.input.'||id FROM embedding_job WHERE source_node_id='beta' AND space_id=?1)", [&active], |r| r.get(0))?;
        for group in ["active", "current", "obsolete"] {
            let mut work: Value = serde_json::from_str(&input).unwrap();
            work["state"] = json!("failed"); work["group"] = json!(group);
            db.execute("INSERT INTO app_meta VALUES(?1,?2)", params![format!("index.bootstrap.{profile}.{group}"),work.to_string()])?;
        }
        db.execute("INSERT INTO app_meta VALUES(?1,?2)", params![format!("index.auto.{profile}.unread"),json!({"node_id":"unread","state":"failed","error":null}).to_string()])?;
        Ok(active)
    }).await.unwrap();
    let before = f.proxy.requests().len();
    let preview = f.service.preview(IndexArgs::default()).await.unwrap();
    assert_eq!(
        (
            preview.sources,
            preview.segments,
            preview.characters,
            preview.unavailable
        ),
        (3, 5, 13, 1)
    );
    let explicit = f
        .service
        .preview(IndexArgs {
            space_id: Some(active),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        (
            explicit.sources,
            explicit.segments,
            explicit.characters,
            explicit.unavailable
        ),
        (3, 4, 11, 1)
    );
    assert!(
        f.service
            .preview(IndexArgs {
                space_id: Some("retired".into()),
                ..Default::default()
            })
            .await
            .is_err()
    );
    assert_eq!(
        f.proxy.requests().len(),
        before,
        "preview has no HTTP requests"
    );
    f.service.retry(IndexArgs::default()).await.unwrap();
    f.db.call(|db| {
        let pending: i64 = db.query_row("SELECT count(*) FROM embedding_job WHERE state='pending'", [], |r| r.get(0))?;
        assert_eq!(pending, 3);
        let retired: String = db.query_row("SELECT state FROM embedding_job WHERE id='retired-job'", [], |r| r.get(0))?;
        assert_eq!(retired, "failed");
        let boot_pending: i64 = db.query_row("SELECT count(*) FROM app_meta WHERE key LIKE 'index.bootstrap.%' AND json_extract(value,'$.state')='pending'", [], |r| r.get(0))?;
        assert_eq!(boot_pending, 2);
        Ok(())
    }).await.unwrap();
    f.close().await;
}
