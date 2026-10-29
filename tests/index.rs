use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shellboy::{
    config::{Config, Connection},
    index::{IndexService, delete_source_vectors_tx, split_text},
    protocol::{IndexArgs, IndexScope},
    storage::DbWorker,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
enum Reply {
    Error(u16, &'static str),
    Embed(usize),
    ProtocolError,
    UncodedError(String),
    Wait(oneshot::Sender<()>, oneshot::Receiver<()>, usize),
}
struct Proxy {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    replies: Arc<Mutex<VecDeque<Reply>>>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
impl Proxy {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let replies = Arc::new(Mutex::new(VecDeque::new()));
        let queued = replies.clone();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = tokio::select! {_=token.cancelled()=>break,result=listener.accept()=>result.unwrap()};
                // Local fixture responses must not depend on delayed ACK/Nagle
                // timing between tiny header and body writes.
                stream.set_nodelay(true).unwrap();
                let mut bytes = Vec::new();
                let split = loop {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..split]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|s| s.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                while bytes.len() < split + length {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).await.unwrap();
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let request: Value = serde_json::from_slice(&bytes[split..split + length]).unwrap();
                captured.lock().unwrap().push(request.clone());
                let reply = queued
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Reply::Embed(3));
                let (status, body) = match reply {
                    Reply::ProtocolError => (200, json!({"invalid":"embedding response"})),
                    Reply::UncodedError(message) => (400, json!({"error":{"message":message}})),
                    Reply::Error(status, code) => (
                        status,
                        json!({"error":{"code":code,"message":"synthetic embedding failure"}}),
                    ),
                    other => {
                        let dimension = match other {
                            Reply::Embed(d) => d,
                            Reply::Wait(entered, release, d) => {
                                let _ = entered.send(());
                                let _ = release.await;
                                d
                            }
                            _ => unreachable!(),
                        };
                        let input = request["input"][0].as_str().unwrap();
                        let coordinate = if input.contains("cat") {
                            0
                        } else if input.contains("dog") {
                            1
                        } else {
                            2
                        };
                        let mut vector = vec![0.0; dimension];
                        vector[coordinate.min(dimension - 1)] = 1.0;
                        (200, json!({"data":[{"index":0,"embedding":vector}]}))
                    }
                };
                let body = serde_json::to_vec(&body).unwrap();
                let head = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut response = head.into_bytes();
                response.extend_from_slice(&body);
                let _ = stream.write_all(&response).await;
            }
        });
        Self {
            url,
            requests,
            replies,
            cancel,
            task,
        }
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    fn push(&self, reply: Reply) {
        self.replies.lock().unwrap().push_back(reply);
    }
    async fn close(self) {
        self.cancel.cancel();
        self.task.await.unwrap();
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    db: DbWorker,
    service: Arc<IndexService>,
    config: Arc<Config>,
    proxy: Proxy,
}
impl Fixture {
    async fn new(config: Config) -> Self {
        Self::with_request_timeout(config, 60_000).await
    }
    async fn with_request_timeout(mut config: Config, request_timeout_ms: u64) -> Self {
        let proxy = Proxy::new().await;
        config.connections = BTreeMap::from([(
            "embed".into(),
            Connection {
                base_url: proxy.url.clone(),
                request_timeout_ms,
                ..Connection::default()
            },
        )]);
        config.embedding.connection = Some("embed".into());
        config.embedding.model = Some("embedding-fixture".into());
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("db.sqlite")).await.unwrap();
        let config = Arc::new(config);
        let service = IndexService::new(db.clone(), config.clone());
        Self {
            _temp: temp,
            db,
            service,
            config,
            proxy,
        }
    }
    async fn node(&self, id: &str, text: &str) {
        self.payload(
            id,
            "user",
            json!({"role":"user","content":[{"type":"input_text","text":text}]}),
        )
        .await;
    }
    async fn payload(&self, id: &str, kind: &str, payload: Value) {
        let id = id.to_owned();
        let kind = kind.to_owned();
        let body = payload.to_string();
        let hash = Sha256::digest(body.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        self.db.call(move|db|{db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,?2,'2026-01-01T00:00:00Z',?3,?4,?5)",params![id,kind,body,body.len()as i64,hash])?;Ok(())}).await.unwrap();
    }
    async fn raw_quote(&self) {
        let body = json!({"item":{"type":"function_call_output","call_id":"call","output":"cat canonical quote"}}).to_string();
        let body_hash = Sha256::digest(body.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.db.call(move |db| { db.execute("UPDATE node SET payload_json=?1,payload_bytes=?2,body_hash=?3 WHERE id='raw-node'", params![body,body.len() as i64,body_hash])?; Ok(()) }).await.unwrap();
    }
    fn raw_gc(&self) -> shellboy::gc::GcService {
        let mut config = Config::default();
        config.retention.tool_raw_bytes = 0;
        let attachments = Arc::new(shellboy::attachments::AttachmentService::new(
            self.db.clone(),
            &config,
            self._temp.path().to_owned(),
            "gc-fixture".into(),
        ));
        shellboy::gc::GcService::new(
            self.db.clone(),
            Arc::new(config),
            self._temp.path().to_owned(),
            attachments,
        )
    }
    async fn drain(&self) {
        for _ in 0..1000 {
            if !self
                .service
                .process_one(CancellationToken::new())
                .await
                .unwrap()
            {
                return;
            }
        }
        panic!("index worker did not settle");
    }
    async fn close(self) {
        self.db.shutdown().await.unwrap();
        self.proxy.close().await;
    }
}
fn args() -> IndexArgs {
    IndexArgs {
        scope: None,
        space_id: None,
        thread_ids: vec![],
        source_ids: vec![],
    }
}
#[tokio::test]
async fn newly_finalized_only_no_implicit_backfill_and_real_vec_search() {
    let f = Fixture::new(Config::default()).await;
    f.node("old", "old dog history").await;
    f.node("new", "new cat answer").await;
    let preview = f.service.enqueue_new(&["new".into()]).await.unwrap();
    assert_eq!(preview.sources, 1);
    assert_eq!(f.proxy.count(), 0);
    f.drain().await;
    let status = f.service.status(args()).await.unwrap();
    assert_eq!(status.done, 1);
    assert!(status.space_id.is_some());
    let matches = f.service.search("cat", 10).await.unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].node_id, "new");
    assert!(matches[0].excerpt.contains("cat"));
    assert_eq!(f.proxy.count(), 2);
    let preview = f
        .service
        .backfill(IndexArgs {
            scope: Some(IndexScope::All),
            ..args()
        })
        .await
        .unwrap();
    assert_eq!(preview.sources, 2);
    f.drain().await;
    assert_eq!(f.service.status(args()).await.unwrap().done, 2);
    assert_eq!(f.proxy.count(), 3);
    f.close().await;
}
#[test]
fn unicode_split_is_bounded_nonoverlapping_and_prefers_paragraphs() {
    let text = "日本語😀\n\nparagraph two long";
    let segments = split_text(text, 7, 0);
    assert_eq!(
        segments.iter().map(|s| s.text.clone()).collect::<String>(),
        text
    );
    assert!(segments.iter().all(|s| s.end - s.start <= 7));
    assert!(segments.windows(2).all(|w| w[0].end == w[1].start));
    assert!(segments[0].text.ends_with("\n\n"));
}
#[tokio::test]
async fn answer_and_reasoning_are_separate_and_opaque_or_binary_is_not_embedded() {
    let f = Fixture::new(Config::default()).await;
    f.payload("assistant","assistant_response",json!({"output":[{"type":"reasoning","encrypted_content":"MUST_NOT_EMBED_OPAQUE","summary":[{"type":"summary_text","text":"reasoning visible"}]},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"cat answer"},{"type":"input_image","image_url":"MUST_NOT_EMBED_BINARY"}]}]})).await;
    f.service.enqueue_new(&["assistant".into()]).await.unwrap();
    f.drain().await;
    {
        let requests = f.proxy.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|r| !r.to_string().contains("MUST_NOT_EMBED"))
        );
        let joined = requests
            .iter()
            .map(|r| r["input"][0].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(joined.contains(&"reasoning visible"));
        assert!(joined.contains(&"cat answer"));
    }
    let kinds =
        f.db.call(|db| {
            let mut statement =
                db.prepare("SELECT DISTINCT body_kind FROM embedding_job ORDER BY body_kind")?;
            Ok(statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(kinds, vec!["answer:1", "reasoning:0"]);
    f.close().await;
}
#[tokio::test]
async fn stale_embedding_result_cannot_resurrect_deleted_source() {
    let f = Fixture::new(Config::default()).await;
    f.node("source", "cat").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    let (entered, started) = oneshot::channel();
    let (release, released) = oneshot::channel();
    f.proxy.push(Reply::Wait(entered, released, 3));
    let service = f.service.clone();
    let task =
        tokio::spawn(async move { service.process_one(CancellationToken::new()).await.unwrap() });
    started.await.unwrap();
    f.db.call(|db|{let tx=db.transaction()?;delete_source_vectors_tx(&tx,"source").unwrap();tx.execute("UPDATE node SET payload_json=NULL,payload_revision=payload_revision+1,deleted_reason='expired' WHERE id='source'",[])?;tx.commit()?;Ok(())}).await.unwrap();
    release.send(()).unwrap();
    assert!(task.await.unwrap());
    assert_eq!(f.service.status(args()).await.unwrap().done, 0);
    let count =
        f.db.call(|db| {
            Ok(db.query_row("SELECT COUNT(*) FROM index_space", [], |r| {
                r.get::<_, i64>(0)
            })?)
        })
        .await
        .unwrap();
    assert_eq!(count, 0);
    f.close().await;
}
#[tokio::test]
async fn auth_pauses_immediately_and_manual_retry_resumes_without_auto_replay() {
    let f = Fixture::new(Config::default()).await;
    f.node("source", "cat").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    f.proxy.push(Reply::Error(401, "invalid_api_key"));
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    let status = f.service.status(args()).await.unwrap();
    assert!(status.paused);
    assert_eq!(status.failed, 1);
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(f.proxy.count(), 1);
    f.service.retry(args()).await.unwrap();
    f.drain().await;
    assert_eq!(f.service.status(args()).await.unwrap().done, 1);
    assert_eq!(f.proxy.count(), 2);
    f.close().await;
}
#[tokio::test]
async fn transient_retry_uses_configured_delay_and_unknown_errors_do_not_split() {
    let mut config = Config::default();
    config.indexing.retry_delays_ms = vec![60_000, 240_000];
    let f = Fixture::new(config).await;
    f.node("source", "long cat text").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    f.proxy.push(Reply::Error(503, "server_error"));
    let before = chrono::Utc::now();
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    let after = chrono::Utc::now();
    f.db.call(move |db| {
        let next: String = db.query_row("SELECT json_extract(value,'$.next_at') FROM app_meta WHERE key LIKE 'index.bootstrap.%'", [], |row| row.get(0))?;
        let next = chrono::DateTime::parse_from_rfc3339(&next).unwrap().with_timezone(&chrono::Utc);
        assert!(next >= before + chrono::Duration::seconds(48));
        assert!(next <= after + chrono::Duration::seconds(72));
        // Make the durable deadline due without depending on scheduler timing.
        db.execute("UPDATE app_meta SET value=json_set(value,'$.next_at','2000-01-01T00:00:00Z') WHERE key LIKE 'index.bootstrap.%'", [])?;
        db.execute("UPDATE embedding_job SET next_attempt_at='2000-01-01T00:00:00Z' WHERE state='pending'", [])?;
        Ok(())
    }).await.unwrap();
    f.drain().await;
    assert_eq!(f.proxy.count(), 2);
    f.node("unknown", "another cat body").await;
    f.service.enqueue_new(&["unknown".into()]).await.unwrap();
    f.proxy.push(Reply::Error(400, "unknown_request"));
    f.drain().await;
    assert_eq!(f.service.status(args()).await.unwrap().failed, 1);
    assert_eq!(f.proxy.count(), 3);
    f.close().await;
}
#[tokio::test]
async fn explicit_length_error_splits_finitely_and_rebuild_failure_preserves_old_space() {
    let f = Fixture::new(Config::default()).await;
    f.node("source", "cat dog long body").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    f.proxy.push(Reply::Error(400, "input_too_long"));
    f.drain().await;
    let old = f.service.status(args()).await.unwrap().space_id.unwrap();
    assert_eq!(f.proxy.count(), 3);
    assert_eq!(f.service.status(args()).await.unwrap().done, 2);
    f.proxy.push(Reply::Error(400, "unknown_provider_failure"));
    f.service
        .rebuild(IndexArgs {
            source_ids: vec!["source".into()],
            ..args()
        })
        .await
        .unwrap();
    f.drain().await;
    assert_eq!(
        f.service.status(args()).await.unwrap().space_id.as_deref(),
        Some(old.as_str())
    );
    assert_eq!(f.service.search("cat", 10).await.unwrap().len(), 1);
    f.close().await;
}
#[tokio::test]
async fn rebuild_success_switches_once_and_all_space_vector_gc_rolls_back_atomically() {
    let f = Fixture::new(Config::default()).await;
    f.node("source", "cat").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    f.drain().await;
    let old = f.service.status(args()).await.unwrap().space_id.unwrap();
    f.service
        .rebuild(IndexArgs {
            source_ids: vec!["source".into()],
            ..args()
        })
        .await
        .unwrap();
    f.drain().await;
    let new = f.service.status(args()).await.unwrap().space_id.unwrap();
    assert_ne!(old, new);
    let counts=f.db.call(|db|{let tx=db.transaction()?;delete_source_vectors_tx(&tx,"source").unwrap();tx.execute("UPDATE node SET payload_json=NULL,payload_revision=payload_revision+1 WHERE id='source'",[])?;tx.rollback()?;let chunks=db.query_row("SELECT COUNT(*) FROM embedding_chunk WHERE source_node_id='source'",[],|r|r.get::<_,i64>(0))?;let mut statement=db.prepare("SELECT state FROM index_space ORDER BY state")?;let states=statement.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;Ok((chunks,states))}).await.unwrap();
    assert_eq!(counts.0, 2);
    assert_eq!(counts.1, vec!["active", "inactive"]);
    f.db.call(|db|{let tx=db.transaction()?;delete_source_vectors_tx(&tx,"source").unwrap();tx.execute("UPDATE node SET payload_json=NULL,payload_revision=payload_revision+1,deleted_reason='expired' WHERE id='source'",[])?;tx.commit()?;Ok(())}).await.unwrap();
    assert!(f.service.search("cat", 10).await.unwrap().is_empty());
    let count=f.db.call(|db|Ok(db.query_row("SELECT COUNT(*) FROM app_meta WHERE key LIKE 'index.preview.%' OR key LIKE 'index.input.%'",[],|r|r.get::<_,i64>(0))?)).await.unwrap();
    assert_eq!(count, 0);
    f.close().await;
}

#[tokio::test]
async fn durable_new_authorization_is_cheap_transactional_and_survives_service_restart() {
    let f = Fixture::new(Config::default()).await;
    f.node("old", "old dog").await;
    f.node("new", "new cat").await;
    let service = f.service.clone();
    f.db.call(move |db| {
        let tx = db.transaction()?;
        service.authorize_new_tx(&tx, &["new".into()]).unwrap();
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(f.proxy.count(), 0);
    assert_eq!(f.service.status(args()).await.unwrap().pending, 1);
    f.drain().await;
    assert_eq!(f.service.status(args()).await.unwrap().done, 1);
    assert_eq!(f.proxy.count(), 1);
    let sources =
        f.db.call(|db| {
            let mut statement = db.prepare("SELECT DISTINCT source_node_id FROM embedding_job")?;
            Ok(statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(sources, vec!["new"]);
    f.close().await;
}
#[tokio::test]
async fn three_exhausted_common_failures_pause_other_jobs_and_unknown_errors_do_not() {
    let mut config = Config::default();
    config.indexing.retries = 0;
    config.indexing.retry_delays_ms = vec![];
    let f = Fixture::new(config).await;
    for n in 0..4 {
        f.node(&format!("node{n}"), "cat").await;
        f.service.enqueue_new(&[format!("node{n}")]).await.unwrap();
    }
    for _ in 0..3 {
        f.proxy.push(Reply::Error(503, "server_error"));
        assert!(
            f.service
                .process_one(CancellationToken::new())
                .await
                .unwrap()
        );
    }
    let status = f.service.status(args()).await.unwrap();
    assert!(status.paused);
    assert_eq!(status.failed, 3);
    assert_eq!(status.pending, 1);
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(f.proxy.count(), 3);
    f.close().await;
}
#[tokio::test]
async fn vector_chunk_and_job_adoption_roll_back_together_on_db_failure() {
    let f = Fixture::new(Config::default()).await;
    f.node("seed", "cat seed").await;
    f.service.enqueue_new(&["seed".into()]).await.unwrap();
    f.drain().await;
    f.node("broken", "dog broken").await;
    f.service.enqueue_new(&["broken".into()]).await.unwrap();
    f.db.call(|db|{db.execute_batch("CREATE TRIGGER inject_index_failure BEFORE INSERT ON embedding_chunk WHEN NEW.source_node_id='broken' BEGIN SELECT RAISE(ABORT,'synthetic adoption failure');END;")?;Ok(())}).await.unwrap();
    f.drain().await;
    let state = f.service.status(args()).await.unwrap();
    assert_eq!(state.done, 1);
    assert_eq!(state.failed, 1);
    let counts =
        f.db.call(|db| {
            let table: String = db.query_row(
                "SELECT vec_table_name FROM index_space WHERE state='active'",
                [],
                |r| r.get(0),
            )?;
            let vectors: i64 =
                db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
            let chunks: i64 =
                db.query_row("SELECT COUNT(*) FROM embedding_chunk", [], |r| r.get(0))?;
            Ok((vectors, chunks))
        })
        .await
        .unwrap();
    assert_eq!(counts, (1, 1));
    f.db.call(|db| {
        db.execute_batch("DROP TRIGGER inject_index_failure;")?;
        Ok(())
    })
    .await
    .unwrap();
    f.service.retry(args()).await.unwrap();
    f.drain().await;
    assert_eq!(f.service.status(args()).await.unwrap().done, 2);
    f.close().await;
}
async fn raw_source(f: &Fixture, bytes: Vec<u8>) {
    f.payload(
        "raw-node",
        "tool_result",
        json!({"item":{"type":"function_call_output","call_id":"call","output":""}}),
    )
    .await;
    f.db.call(move|db|{db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('raw-thread','/tmp','/tmp/AGENTS.md','embed','model','now')",[])?;db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('raw-run','raw-thread','completed','now')",[])?;db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('raw-turn','raw-thread','raw-run','completed','now')",[])?;db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('raw-attempt','raw-turn',0,0,'embed','model',0,'completed','{}','now')",[])?;db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('raw-group','raw-attempt','raw-turn','completed','now')",[])?;db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('raw-group','call',0,'shell','{}','completed','execution')",[])?;db.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('raw-group','call','raw-node','completed')",[])?;db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at)VALUES('execution','raw-run','raw-turn','execution','exited','/tmp','{}','UTF-8','UTF-8','closed',1,0,1,'now','now')",[])?;db.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('execution',0,'stdout',0,?1,'now')",[bytes])?;Ok(())}).await.unwrap();
}
#[tokio::test]
async fn raw_gc_keeps_canonical_quote_and_attachment_vectors_including_legacy_witnesses() {
    use shellboy::{attachments::AttachmentService, gc::GcService, protocol::StorageCategory};
    let f = Fixture::new(Config::default()).await;
    raw_source(&f, b"dog raw bytes only".to_vec()).await;
    let body = json!({"item":{"type":"function_call_output","call_id":"call","output":"cat canonical quote"}}).to_string();
    let body_hash = Sha256::digest(body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    f.db.call(move |db| {
        db.execute("UPDATE node SET payload_json=?1,payload_bytes=?2,body_hash=?3 WHERE id='raw-node'", params![body,body.len() as i64,body_hash])?;
        db.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at) VALUES('raw-attachment','raw-node','/fixture/cat-attachment.txt','file','external','now')", [])?;
        Ok(())
    }).await.unwrap();
    f.service.enqueue_tool_raw("raw-node").await.unwrap();
    f.drain().await;
    assert_eq!(f.service.status(args()).await.unwrap().done, 3);
    let old_space = f.service.status(args()).await.unwrap().space_id;
    f.service
        .rebuild(IndexArgs {
            source_ids: vec!["raw-node".into()],
            ..args()
        })
        .await
        .unwrap();
    f.drain().await;
    assert_ne!(f.service.status(args()).await.unwrap().space_id, old_space);
    // Old visible jobs carried the same raw witness as tool_raw jobs. Their
    // already-paid canonical chunks must remain usable without re-embedding.
    f.db.call(|db| {
        db.execute("UPDATE embedding_job SET authorized_watermark=json_set(authorized_watermark,'$.witness.raw',json((SELECT json_extract(authorized_watermark,'$.witness.raw') FROM embedding_job WHERE body_kind='tool_raw' LIMIT 1))) WHERE body_kind<>'tool_raw'", [])?;
        Ok(())
    }).await.unwrap();
    let before = f.proxy.count();
    let canonical =
        f.db.call(|db| {
            Ok(db.query_row(
                "SELECT payload_json,body_hash,payload_revision FROM node WHERE id='raw-node'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )?)
        })
        .await
        .unwrap();
    let mut config = Config::default();
    config.retention.tool_raw_bytes = 0;
    let attachments = Arc::new(AttachmentService::new(
        f.db.clone(),
        &config,
        f._temp.path().to_owned(),
        "gc-fixture".into(),
    ));
    let gc = GcService::new(
        f.db.clone(),
        Arc::new(config),
        f._temp.path().to_owned(),
        attachments,
    );
    f.db.call(|db| { db.execute_batch("CREATE TRIGGER reject_raw_delete BEFORE DELETE ON embedding_job WHEN OLD.body_kind='tool_raw' BEGIN SELECT RAISE(ABORT,'fixture raw deletion failure'); END;")?; Ok(()) }).await.unwrap();
    assert!(
        gc.collect(StorageCategory::Raw, chrono::Utc::now())
            .await
            .is_err()
    );
    let unchanged =
        f.db.call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM embedding_chunk", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row(
                    "SELECT retention_generation FROM shell_execution WHERE id='execution'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
                db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key='raw_tombstone.execution'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(
        unchanged,
        (6, 0, 0),
        "vector deletion, marker and generation must roll back together"
    );
    f.db.call(|db| {
        db.execute_batch("DROP TRIGGER reject_raw_delete")?;
        Ok(())
    })
    .await
    .unwrap();
    gc.collect(StorageCategory::Raw, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(
        f.proxy.count(),
        before,
        "GC must not call the embedding API"
    );
    let counts =
        f.db.call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM output_chunk", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row("SELECT count(*) FROM embedding_chunk", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row(
                    "SELECT count(*) FROM embedding_job WHERE body_kind='tool_raw'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(
        counts,
        (0, 4, 0),
        "raw retention must preserve both canonical bodies"
    );
    let after =
        f.db.call(|db| {
            Ok(db.query_row(
                "SELECT payload_json,body_hash,payload_revision FROM node WHERE id='raw-node'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        after, canonical,
        "raw retention must not mutate canonical history"
    );
    let hits = f.service.search_detailed("cat", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0]
            .spans
            .iter()
            .any(|span| span.body_kind == "tool_result")
    );
    assert!(
        hits[0]
            .spans
            .iter()
            .any(|span| span.body_kind == "attachment_paths")
    );
    let before = f.proxy.count();
    let preview = f.service.enqueue_tool_raw("raw-node").await.unwrap();
    assert_eq!(preview.unavailable, 0);
    assert_eq!(preview.already_indexed, 2);
    assert_eq!(preview.segments, 0);
    f.drain().await;
    assert_eq!(
        f.proxy.count(),
        before,
        "canonical jobs must not be paid for twice"
    );
    let management = f
        .service
        .management_status(IndexArgs {
            source_ids: vec!["raw-node".into()],
            ..args()
        })
        .await
        .unwrap();
    assert_eq!(management["coverage"]["fully_indexed_sources"], 1);
    assert_eq!(management["coverage"]["coverage_unverified_sources"], 0);
    f.db.call(|db| {
        let tx = db.transaction()?;
        delete_source_vectors_tx(&tx, "raw-node").unwrap();
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.service.status(args()).await.unwrap().done,
        0,
        "full source deletion still removes canonical jobs"
    );
    f.close().await;
}
#[tokio::test]
async fn legacy_canonical_bootstrap_inflight_survives_raw_gc() {
    let f = Fixture::new(Config::default()).await;
    raw_source(&f, b"dog original raw".to_vec()).await;
    f.raw_quote().await;
    f.service.enqueue_tool_raw("raw-node").await.unwrap();
    f.db.call(|db| {
        // A pre-upgrade canonical authorization carried the raw witness too.
        db.execute("UPDATE app_meta SET value=json_set(value,'$.witness.raw',json((SELECT json_extract(value,'$.witness.raw') FROM app_meta WHERE key LIKE 'index.bootstrap.%' AND json_extract(value,'$.body_kind')='tool_raw' LIMIT 1))) WHERE key LIKE 'index.bootstrap.%' AND json_extract(value,'$.body_kind')='tool_result'", [])?;
        db.execute("UPDATE app_meta SET value=json_set(value,'$.state','failed') WHERE key LIKE 'index.bootstrap.%' AND json_extract(value,'$.body_kind')='tool_raw'", [])?;
        Ok(())
    }).await.unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, hold) = oneshot::channel();
    f.proxy.push(Reply::Wait(entered, hold, 3));
    let service = f.service.clone();
    let running = tokio::spawn(async move { service.process_one(CancellationToken::new()).await });
    ready.await.unwrap();
    assert!(
        f.proxy.requests.lock().unwrap()[0]["input"][0]
            .as_str()
            .unwrap()
            .contains("cat canonical")
    );
    f.raw_gc()
        .collect(shellboy::protocol::StorageCategory::Raw, chrono::Utc::now())
        .await
        .unwrap();
    release.send(()).unwrap();
    running.await.unwrap().unwrap();
    f.drain().await;
    assert_eq!(derived_counts(&f.db).await, (1, 1, 1));
    assert_eq!(f.proxy.count(), 1);
    let hits = f.service.search_detailed("cat", 10).await.unwrap();
    assert_eq!(hits[0].spans[0].body_kind, "tool_result");
    f.close().await;
}
#[tokio::test]
async fn encoding_change_preserves_canonical_quote_without_reembedding() {
    let f = Fixture::new(Config::default()).await;
    raw_source(&f, b"dog raw".to_vec()).await;
    f.raw_quote().await;
    f.service.enqueue_tool_raw("raw-node").await.unwrap();
    f.drain().await;
    assert_eq!(derived_counts(&f.db).await, (2, 2, 2));
    let before = f.proxy.count();
    let reader =
        shellboy::control::output::ShellOutputReader::new(f.db.clone(), "canonical-encoding");
    reader.read(encoding_read("cp932")).await.unwrap();
    assert_eq!(derived_counts(&f.db).await, (1, 1, 1));
    assert_eq!(f.proxy.count(), before);
    let hits = f.service.search_detailed("cat", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].spans.len(), 1);
    assert_eq!(hits[0].spans[0].body_kind, "tool_result");
    f.close().await;
}
#[tokio::test]
async fn full_raw_middle_is_indexed_in_bounded_segments_and_gc_marker_blocks_new_work() {
    let mut config = Config::default();
    config.indexing.split_chars = 1000;
    let f = Fixture::new(config).await;
    let text = format!(
        "head {} unique_middle_needle dog {} tail",
        "pad ".repeat(20_000),
        "pad ".repeat(20_000)
    );
    raw_source(&f, text.into_bytes()).await;
    let preview = f.service.enqueue_tool_raw("raw-node").await.unwrap();
    assert!(preview.segments > 100);
    f.drain().await;
    assert!(f.proxy.requests.lock().unwrap().iter().any(|r| {
        r["input"][0]
            .as_str()
            .unwrap()
            .contains("unique_middle_needle")
    }));
    assert!(
        f.proxy
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r["input"][0].as_str().unwrap().chars().count() <= 1000)
    );
    f.db.call(|db|{let tx=db.transaction()?;tx.execute("INSERT INTO app_meta(key,value)VALUES('raw_tombstone.execution','{\"state\":\"deleting\"}')",[])?;tx.execute("UPDATE shell_execution SET retention_generation=retention_generation+1 WHERE id='execution'",[])?;delete_source_vectors_tx(&tx,"raw-node").unwrap();tx.execute("DELETE FROM output_chunk WHERE execution_id='execution'",[])?;tx.commit()?;Ok(())}).await.unwrap();
    let preview = f.service.enqueue_tool_raw("raw-node").await.unwrap();
    assert_eq!(preview.unavailable, 0);
    assert_eq!(preview.segments, 0);
    assert_eq!(f.service.status(args()).await.unwrap().done, 0);
    f.close().await;
}
#[tokio::test]
async fn raw_retention_during_inflight_embedding_does_not_resurrect_vectors() {
    let f = Fixture::new(Config::default()).await;
    raw_source(&f, b"cat full raw".to_vec()).await;
    f.service.enqueue_tool_raw("raw-node").await.unwrap();
    let (entered, started) = oneshot::channel();
    let (release, released) = oneshot::channel();
    f.proxy.push(Reply::Wait(entered, released, 3));
    let service = f.service.clone();
    let task =
        tokio::spawn(async move { service.process_one(CancellationToken::new()).await.unwrap() });
    started.await.unwrap();
    f.raw_gc()
        .collect(shellboy::protocol::StorageCategory::Raw, chrono::Utc::now())
        .await
        .unwrap();
    release.send(()).unwrap();
    task.await.unwrap();
    assert_eq!(f.service.status(args()).await.unwrap().done, 0);
    assert_eq!(
        f.service
            .enqueue_tool_raw("raw-node")
            .await
            .unwrap()
            .unavailable,
        0
    );
    let management = f
        .service
        .management_status(IndexArgs {
            source_ids: vec!["raw-node".into()],
            ..args()
        })
        .await
        .unwrap();
    assert_eq!(management["coverage"]["not_indexable_sources"], 1);
    assert_eq!(management["coverage"]["eligible_sources"], 0);
    f.close().await;
}

#[tokio::test]
async fn partially_failed_candidate_is_visible_and_retry_completes_before_switch() {
    let f = Fixture::new(Config::default()).await;
    f.node("cat-source", "cat").await;
    f.node("dog-source", "dog").await;
    f.service
        .enqueue_new(&["cat-source".into(), "dog-source".into()])
        .await
        .unwrap();
    f.drain().await;
    let old = f.service.status(args()).await.unwrap().space_id.unwrap();
    f.proxy.push(Reply::Embed(3));
    f.proxy.push(Reply::Error(400, "unknown_failure"));
    f.service
        .rebuild(IndexArgs {
            source_ids: vec!["cat-source".into(), "dog-source".into()],
            ..args()
        })
        .await
        .unwrap();
    f.drain().await;
    let status = f.service.status(args()).await.unwrap();
    assert_eq!(status.space_id.as_deref(), Some(old.as_str()));
    assert_eq!(status.failed, 1);
    f.service.retry(args()).await.unwrap();
    f.drain().await;
    let status = f.service.status(args()).await.unwrap();
    assert_ne!(status.space_id.as_deref(), Some(old.as_str()));
    assert_eq!(status.failed, 0);
    assert_eq!(status.done, 2);
    f.close().await;
}

#[tokio::test]
async fn logical_quota_pauses_index_work_but_preserves_semantic_reads_and_pause_state() {
    let mut config = Config::default();
    config.limits.data_warning_bytes = 1000;
    config.limits.data_max_bytes = 3000;
    let policy = config.clone();
    let f = Fixture::new(config).await;
    f.node("filler", &"filler ".repeat(1000)).await;
    f.node("source", "cat").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(f.proxy.count(), 0);
    assert!(f.service.status(args()).await.unwrap().paused);
    f.db.call(move|db|{db.execute("UPDATE node SET payload_json=NULL,payload_bytes=0,deleted_reason='test cleanup' WHERE id='filler'",[])?;assert!(shellboy::gc::require_capacity_tx(db,&policy).is_ok());Ok(())}).await.unwrap();
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(f.proxy.count(), 0);
    f.service.retry(args()).await.unwrap();
    f.drain().await;
    assert_eq!(f.service.status(args()).await.unwrap().done, 1);
    assert_eq!(f.proxy.count(), 1);
    f.node("query-filler", &"filler ".repeat(1000)).await;
    let before =
        f.db.call(|db| {
            Ok(db
                .prepare("SELECT key,value FROM app_meta WHERE key LIKE 'index.%' ORDER BY key")?
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    let hits = f.service.search("cat", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_id, "source");
    assert_eq!(
        f.proxy.count(),
        2,
        "only the explicitly requested query embedding is generated"
    );
    let after =
        f.db.call(|db| {
            Ok(db
                .prepare("SELECT key,value FROM app_meta WHERE key LIKE 'index.%' ORDER BY key")?
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "search must not pause the index or create derived records"
    );
    assert!(!f.service.status(args()).await.unwrap().paused);
    f.node("pending-source", "dog").await;
    f.service
        .enqueue_new(&["pending-source".into()])
        .await
        .unwrap();
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert!(f.service.status(args()).await.unwrap().paused);
    let paused =
        f.db.call(|db| {
            Ok(db
                .prepare(
                    "SELECT key,value FROM app_meta WHERE key LIKE 'index.pause.%' ORDER BY key",
                )?
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(f.proxy.count(), 2);
    assert_eq!(f.service.search("cat", 10).await.unwrap().len(), 1);
    assert_eq!(f.proxy.count(), 3);
    let after =
        f.db.call(|db| {
            Ok(db
                .prepare(
                    "SELECT key,value FROM app_meta WHERE key LIKE 'index.pause.%' ORDER BY key",
                )?
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(
        after, paused,
        "a query must not resume or replace the saved pause"
    );
    assert_eq!(f.service.status(args()).await.unwrap().pending, 1);
    f.close().await;
}

#[tokio::test]
async fn new_finalized_sources_during_rebuild_are_caught_up_before_switch() {
    let f = Fixture::new(Config::default()).await;
    f.node("old", "cat old").await;
    f.service.enqueue_new(&["old".into()]).await.unwrap();
    f.drain().await;
    let old = f.service.status(args()).await.unwrap().space_id.unwrap();
    f.service
        .rebuild(IndexArgs {
            source_ids: vec!["old".into()],
            ..args()
        })
        .await
        .unwrap();
    f.node("new", "dog arrived while rebuilding").await;
    let service = f.service.clone();
    f.db.call(move |db| {
        let tx = db.transaction()?;
        service.authorize_new_tx(&tx, &["new".into()]).unwrap();
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    f.drain().await;
    let status = f.service.status(args()).await.unwrap();
    assert_ne!(status.space_id.as_deref(), Some(old.as_str()));
    assert_eq!(status.done, 2);
    assert!(
        f.service
            .search("dog", 10)
            .await
            .unwrap()
            .iter()
            .any(|hit| hit.node_id == "new")
    );
    f.close().await;
}

#[tokio::test]
async fn management_coverage_is_scoped_read_only_and_rejects_stale_versions() {
    let mut config = Config::default();
    config.indexing.split_chars = 10;
    let f = Fixture::new(config).await;
    f.node("done", "cat").await;
    f.node("partial", "cat aaa bbb ccc ddd eee fff ggg").await;
    f.node("untouched", "dog").await;
    f.payload(
        "opaque",
        "assistant_response",
        json!({"output":[{"type":"reasoning","encrypted_content":"opaque","summary":[]}]}),
    )
    .await;
    f.service.enqueue_new(&["done".into()]).await.unwrap();
    f.drain().await;
    f.service.enqueue_new(&["partial".into()]).await.unwrap();
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    f.db.call(|db|{db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('view','/tmp','/tmp/AGENTS.md','embed','model','now')",[])?;db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('view',0,'done'),('view',1,'untouched')",[])?;Ok(())}).await.unwrap();
    let before = f.db.call(|db| Ok(db.total_changes())).await.unwrap();
    let calls = f.proxy.count();
    let status = f.service.management_status(args()).await.unwrap();
    assert_eq!(status["coverage"]["eligible_sources"], 3);
    assert_eq!(status["coverage"]["fully_indexed_sources"], 1);
    assert_eq!(status["coverage"]["partially_indexed_sources"], 1);
    assert_eq!(status["coverage"]["not_indexed_sources"], 1);
    assert_eq!(status["coverage"]["not_indexable_sources"], 1);
    assert_eq!(status["spaces"].as_array().unwrap().len(), 1);
    assert_eq!(status["spaces"][0]["dimension"], 3);
    assert_eq!(status["spaces"][0]["selected"], true);
    let selected = f
        .service
        .management_status(IndexArgs {
            thread_ids: vec!["view".into()],
            ..args()
        })
        .await
        .unwrap();
    assert_eq!(selected["coverage"]["eligible_sources"], 2);
    assert_eq!(selected["coverage"]["partially_indexed_sources"], 0);
    assert_eq!(
        f.db.call(|db| Ok(db.total_changes())).await.unwrap(),
        before
    );
    assert_eq!(f.proxy.count(), calls);
    let preview = f
        .service
        .preview(IndexArgs {
            scope: Some(IndexScope::All),
            ..args()
        })
        .await
        .unwrap();
    assert_eq!(preview.already_indexed, 2);
    assert!(preview.segments > preview.already_indexed);
    assert_eq!(f.proxy.count(), calls);
    f.db.call(|db| {
        db.execute(
            "UPDATE node SET payload_revision=payload_revision+1 WHERE id='done'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let status = f
        .service
        .management_status(IndexArgs {
            source_ids: vec!["done".into()],
            ..args()
        })
        .await
        .unwrap();
    assert_eq!(status["coverage"]["not_indexed_sources"], 1);
    assert_eq!(status["coverage"]["fully_indexed_sources"], 0);
    assert_eq!(status["coverage"]["stale_segments"], 1);
    assert_eq!(
        f.service
            .preview(IndexArgs {
                source_ids: vec!["done".into()],
                ..args()
            })
            .await
            .unwrap()
            .already_indexed,
        0
    );
    f.close().await;
}

#[tokio::test]
async fn management_keeps_raw_coverage_unverified_and_space_history_visible() {
    let f = Fixture::new(Config::default()).await;
    raw_source(&f, b"cat retained output".to_vec()).await;
    f.service.enqueue_tool_raw("raw-node").await.unwrap();
    f.drain().await;
    let old = f.service.status(args()).await.unwrap().space_id.unwrap();
    let before = f.db.call(|db| Ok(db.total_changes())).await.unwrap();
    let calls = f.proxy.count();
    let status = f.service.management_status(args()).await.unwrap();
    assert_eq!(status["coverage"]["coverage_unverified_sources"], 1);
    assert_eq!(status["coverage"]["fully_indexed_sources"], 0);
    assert_eq!(
        f.db.call(|db| Ok(db.total_changes())).await.unwrap(),
        before
    );
    assert_eq!(f.proxy.count(), calls);
    f.service
        .rebuild(IndexArgs {
            source_ids: vec!["raw-node".into()],
            ..args()
        })
        .await
        .unwrap();
    f.drain().await;
    let status = f.service.management_status(args()).await.unwrap();
    assert_eq!(status["spaces"].as_array().unwrap().len(), 2);
    assert_ne!(status["selected_space"], old);
    assert!(
        status["spaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|space| space["id"] == old && space["state"] == "inactive")
    );
    let explicit = f
        .service
        .management_status(IndexArgs {
            space_id: Some(old.clone()),
            source_ids: vec!["raw-node".into()],
            ..args()
        })
        .await
        .unwrap();
    assert_eq!(explicit["selected_space"], old);
    assert_eq!(explicit["coverage"]["coverage_unverified_sources"], 1);
    f.db.call(|db|{db.execute("UPDATE shell_execution SET retention_generation=retention_generation+1 WHERE id='execution'",[])?;Ok(())}).await.unwrap();
    let status = f.service.management_status(args()).await.unwrap();
    assert_eq!(status["coverage"]["not_indexed_sources"], 1);
    assert!(status["coverage"]["stale_segments"].as_u64().unwrap() > 0);
    f.close().await;
}

#[tokio::test]
async fn fee_preview_credits_completed_length_split_children_without_api_or_new_jobs() {
    let f = Fixture::new(Config::default()).await;
    f.node("source", "cat dog long body").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    f.proxy.push(Reply::Error(400, "input_too_long"));
    f.drain().await;
    let calls = f.proxy.count();
    let before = f.db.call(|db| Ok(db.total_changes())).await.unwrap();
    let preview = f
        .service
        .preview(IndexArgs {
            source_ids: vec!["source".into()],
            ..args()
        })
        .await
        .unwrap();
    assert_eq!(preview.segments, 1);
    assert_eq!(preview.already_indexed, 1);
    assert_eq!(f.proxy.count(), calls);
    assert_eq!(
        f.db.call(|db| Ok(db.total_changes())).await.unwrap(),
        before
    );
    let status = f.service.management_status(args()).await.unwrap();
    assert_eq!(status["coverage"]["fully_indexed_sources"], 1);
    f.close().await;
}

#[tokio::test]
async fn interrupted_rebuild_fence_stays_visible_across_retry_and_restart() {
    let f = Fixture::new(Config::default()).await;
    f.node("source", "cat").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    f.drain().await;
    let active = f.service.status(args()).await.unwrap().space_id.unwrap();
    f.service
        .rebuild(IndexArgs {
            source_ids: vec!["source".into()],
            ..args()
        })
        .await
        .unwrap();
    f.db.call(|db|{db.execute("INSERT INTO app_meta(key,value) SELECT replace(key,'index.rebuild.','index.enqueueing.')||'.'||value,'1' FROM app_meta WHERE key LIKE 'index.rebuild.%'",[])?;Ok(())}).await.unwrap();
    f.drain().await;
    assert_eq!(
        f.service.status(args()).await.unwrap().space_id.as_deref(),
        Some(active.as_str())
    );
    let view = f.service.management_status(args()).await.unwrap();
    assert_eq!(view["preparation"]["enqueue_fence_present"], true);
    assert_eq!(view["preparation"]["enqueue_fence_count"], 1);
    assert!(
        f.service
            .status(args())
            .await
            .unwrap()
            .reason
            .unwrap()
            .contains("admission")
    );
    f.service.retry(args()).await.unwrap();
    let mut config = Config {
        connections: BTreeMap::from([(
            "embed".into(),
            Connection {
                base_url: f.proxy.url.clone(),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    config.embedding.connection = Some("embed".into());
    config.embedding.model = Some("embedding-fixture".into());
    let restarted = IndexService::new(f.db.clone(), Arc::new(config));
    let workers = restarted.start().await.unwrap();
    assert_eq!(
        restarted.management_status(args()).await.unwrap()["preparation"]["enqueue_fence_present"],
        true
    );
    assert_eq!(
        restarted.status(args()).await.unwrap().space_id.as_deref(),
        Some(active.as_str())
    );
    workers.shutdown().await;
    restarted.wait_idle().await.unwrap();
    assert_eq!(f.proxy.count(), 2);
    f.close().await;
}

#[tokio::test]
async fn index_context_is_transactional_deduplicated_and_never_copies_provider_secrets() {
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db|{db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('observer','/tmp','/tmp/AGENTS.md','embed','model','now')",[])?;Ok(())}).await.unwrap();
    let service = f.service.clone();
    f.db.call(move |db| {
        let tx = db.transaction()?;
        service
            .record_context_observations_tx(&tx, "observer", "2026-10-24T00:00:00Z")
            .unwrap();
        tx.rollback()?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.db.call(|db| Ok(db.query_row("SELECT COUNT(*) FROM event", [], |r| r.get::<_, i64>(0))?))
            .await
            .unwrap(),
        0
    );
    let service = f.service.clone();
    f.db.call(move |db| {
        let tx = db.transaction()?;
        service
            .record_context_observations_tx(&tx, "observer", "2026-10-24T00:01:00Z")
            .unwrap();
        service
            .record_context_observations_tx(&tx, "observer", "2026-10-24T00:02:00Z")
            .unwrap();
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.db.call(|db| Ok(db.query_row("SELECT COUNT(*) FROM event", [], |r| r.get::<_, i64>(0))?))
            .await
            .unwrap(),
        1
    );
    f.node("source", "cat").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    f.drain().await;
    let service = f.service.clone();
    f.db.call(move|db|{let tx=db.transaction()?;service.record_context_observations_tx(&tx,"observer","2026-10-24T00:03:00Z").unwrap();tx.execute("INSERT INTO app_meta(key,value) SELECT 'index.pause.'||substr(splitter_version,instr(splitter_version,':')+1),?1 FROM index_space WHERE state='active'",[json!({"kind":"provider","message":"MUST_NOT_COPY_SECRET","provider_code":"private"}).to_string()])?;service.record_context_observations_tx(&tx,"observer","2026-10-24T00:04:00Z").unwrap();tx.commit()?;Ok(())}).await.unwrap();
    let payloads =
        f.db.call(|db| {
            let mut statement =
                db.prepare("SELECT payload_ref_json FROM event ORDER BY created_at,id")?;
            Ok(statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(payloads.len(), 3);
    assert!(payloads.iter().all(|p| !p.contains("MUST_NOT_COPY_SECRET")
        && !p.contains("provider_code")
        && !p.contains("http://")));
    let last: Value = serde_json::from_str(payloads.last().unwrap()).unwrap();
    assert_eq!(last["kind"], "index_state");
    assert_eq!(last["change"]["after"]["pause_kind"], "provider");
    assert!(last.get("timezone").is_some());
    assert_eq!(f.proxy.count(), 1);
    f.close().await;
}

fn encoding_read(encoding: &str) -> shellboy::protocol::ShellReadArgs {
    shellboy::protocol::ShellReadArgs {
        execution_id: "execution".into(),
        cursor: None,
        encoding: Some(encoding.into()),
        limit_bytes: 65536,
    }
}
async fn derived_counts(db: &DbWorker) -> (i64, i64, i64) {
    db.call(|db| {
        let jobs = db.query_row("SELECT COUNT(*) FROM embedding_job", [], |r| r.get(0))?;
        let chunks = db.query_row("SELECT COUNT(*) FROM embedding_chunk", [], |r| r.get(0))?;
        let mut statement = db.prepare("SELECT vec_table_name FROM index_space")?;
        let tables = statement
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut vectors = 0;
        for table in tables {
            vectors += db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })?;
        }
        Ok((jobs, chunks, vectors))
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn decoding_change_atomically_invalidates_all_spaces_without_touching_raw_or_generation() {
    let f = Fixture::new(Config::default()).await;
    raw_source(&f, b"cat ASCII output".to_vec()).await;
    f.service.enqueue_tool_raw("raw-node").await.unwrap();
    f.drain().await;
    f.service
        .rebuild(IndexArgs {
            source_ids: vec!["raw-node".into()],
            ..args()
        })
        .await
        .unwrap();
    f.drain().await;
    assert_eq!(derived_counts(&f.db).await, (2, 2, 2));
    let reader = shellboy::control::output::ShellOutputReader::new(f.db.clone(), "encoding-check");
    reader.read(encoding_read("utf8")).await.unwrap();
    assert_eq!(derived_counts(&f.db).await, (2, 2, 2));
    f.db.call(|db|{db.execute_batch("CREATE TRIGGER reject_encoding BEFORE UPDATE OF output_encoding ON shell_execution BEGIN SELECT RAISE(ABORT,'injected encoding adoption failure');END;")?;Ok(())}).await.unwrap();
    assert!(reader.read(encoding_read("cp932")).await.is_err());
    assert_eq!(derived_counts(&f.db).await, (2, 2, 2));
    f.db.call(|db| {
        let state: (String, i64) = db.query_row(
            "SELECT output_encoding,retention_generation FROM shell_execution WHERE id='execution'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(state, ("UTF-8".into(), 0));
        db.execute_batch("DROP TRIGGER reject_encoding;")?;
        Ok(())
    })
    .await
    .unwrap();
    let output = reader.read(encoding_read("cp932")).await.unwrap();
    assert_eq!(output.text, "cat ASCII output");
    assert_eq!(derived_counts(&f.db).await, (0, 0, 0));
    f.db.call(|db| {
        let raw: Vec<u8> = db.query_row(
            "SELECT bytes FROM output_chunk WHERE execution_id='execution'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(raw, b"cat ASCII output");
        assert_eq!(
            db.query_row(
                "SELECT retention_generation FROM shell_execution WHERE id='execution'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(f.proxy.count(), 2);
    let added = f.service.enqueue_tool_raw("raw-node").await.unwrap();
    assert_eq!(added.segments, 1);
    f.drain().await;
    assert_eq!(derived_counts(&f.db).await, (1, 1, 1));
    assert_eq!(f.proxy.count(), 3);
    reader.read(encoding_read("Shift_JIS")).await.unwrap();
    assert_eq!(derived_counts(&f.db).await, (1, 1, 1));
    f.close().await;
}
#[tokio::test]
async fn decoding_change_rejects_inflight_result_and_ascii_can_be_explicitly_reindexed() {
    let f = Fixture::new(Config::default()).await;
    raw_source(&f, b"cat unchanged ASCII".to_vec()).await;
    f.service.enqueue_tool_raw("raw-node").await.unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, hold) = oneshot::channel();
    f.proxy.push(Reply::Wait(entered, hold, 3));
    let service = f.service.clone();
    let running = tokio::spawn(async move { service.process_one(CancellationToken::new()).await });
    ready.await.unwrap();
    let reader = shellboy::control::output::ShellOutputReader::new(f.db.clone(), "encoding-late");
    reader.read(encoding_read("cp932")).await.unwrap();
    release.send(()).unwrap();
    running.await.unwrap().unwrap();
    assert_eq!(derived_counts(&f.db).await, (0, 0, 0));
    assert_eq!(f.proxy.count(), 1);
    f.service.enqueue_tool_raw("raw-node").await.unwrap();
    f.drain().await;
    assert_eq!(derived_counts(&f.db).await, (1, 1, 1));
    assert_eq!(f.proxy.count(), 2);
    f.close().await;
}

fn search_args(query: &str, limit: u32) -> shellboy::protocol::HistorySearchArgs {
    shellboy::protocol::HistorySearchArgs {
        query: query.into(),
        limit,
        ..Default::default()
    }
}
#[tokio::test]
async fn search_filters_precede_knn_even_beyond_four_hundred_nearer_chunks() {
    let f = Fixture::new(Config::default()).await;
    let mut ids = Vec::new();
    for i in 0..405 {
        let id = format!("near-{i:03}");
        f.node(&id, "cat nearby but excluded").await;
        ids.push(id);
    }
    f.payload("far", "assistant_response", json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"dog valid distant result"}]}]})).await;
    ids.push("far".into());
    f.service.enqueue_new(&ids).await.unwrap();
    f.drain().await;
    let mut query = search_args("cat", 1);
    query.kinds = vec!["assistant_response".into()];
    let hits = f.service.search_filtered(&query).await.unwrap();
    assert!(hits.iter().all(|hit| hit.thread_scope.is_none()));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_id, "far");
    assert_eq!(hits[0].provenance.kind, "assistant_response");
    assert_eq!(hits[0].provenance.source_revision, 0);
    assert!(hits[0].provenance.origin_thread_id.is_none());
    assert!(
        hits[0]
            .spans
            .iter()
            .all(|span| span.source_revision == 0 && span.content_hash.len() == 64)
    );
    query.kinds.clear();
    query.exclude_kinds = vec!["user".into()];
    assert_eq!(
        f.service.search_filtered(&query).await.unwrap()[0].node_id,
        "far"
    );
    f.close().await;
}
#[tokio::test]
async fn search_scope_is_selected_path_plus_transitive_completed_replacement_sources() {
    let f = Fixture::new(Config::default()).await;
    for id in [
        "shared",
        "old-source",
        "nested-summary",
        "summary",
        "off-path",
        "pending-source",
    ] {
        f.node(id, "cat history").await;
    }
    f.service
        .enqueue_new(&[
            "shared".into(),
            "old-source".into(),
            "nested-summary".into(),
            "summary".into(),
            "off-path".into(),
            "pending-source".into(),
        ])
        .await
        .unwrap();
    f.drain().await;
    f.db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('parent','/','/a','p','m','now'),('fork','/','/a','p','m','now');
        UPDATE thread SET parent_thread_id='parent' WHERE id='fork';
        INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('parent',0,'shared'),('parent',1,'off-path'),('fork',0,'shared'),('fork',1,'summary');
        INSERT INTO edge(src,dst) VALUES('off-path','summary');
        INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES('r','summary','k','1','1','completed','now'),('nested','nested-summary','k2','1','1','completed','now'),('pending','summary','k3','1','1','pending','now');
        INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision) VALUES('r',0,'nested-summary',0),('nested',0,'old-source',0),('pending',0,'pending-source',0);")?;
        Ok(())
    }).await.unwrap();
    let mut query = search_args("cat", 50);
    query.thread_id = Some("fork".into());
    let hits = f.service.search_filtered(&query).await.unwrap();
    for hit in &hits {
        let scope = hit.thread_scope.as_ref().unwrap();
        assert_eq!(scope.thread_id, "fork");
        assert_eq!(
            scope.on_current_path,
            matches!(hit.node_id.as_str(), "shared" | "summary")
        );
    }
    let mut ids = hits.into_iter().map(|h| h.node_id).collect::<Vec<_>>();
    ids.sort();
    assert_eq!(
        ids,
        vec!["nested-summary", "old-source", "shared", "summary"]
    );
    // The independent keyword adapter observes precisely the same membership,
    // even with no embedding configuration or selected knowledge database.
    let data = f._temp.path().join("keyword-data");
    std::fs::create_dir(&data).unwrap();
    let history = data.join("shellboy.db");
    f.db.call(move |db| {
        db.execute("VACUUM INTO ?1", [history.to_string_lossy().as_ref()])?;
        Ok(())
    })
    .await
    .unwrap();
    let keyword = shellboy::rag::execute(
        shellboy::rag::RagOptions {
            db: Some(f._temp.path().join("unused-rag.db")),
            command: shellboy::rag::RagCommand::Search {
                query: "cat".into(),
                scope: vec!["history:fork".into()],
                mode: shellboy::rag::SearchMode::Keyword,
                limit: 100,
            },
        },
        Some(f._temp.path().join("absent-config.toml")),
        Some(data),
        f._temp.path(),
    )
    .await;
    assert_eq!(keyword["ok"], true, "{keyword}");
    let mut keyword_ids = keyword["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| {
            let id = hit["location"]["node_id"].as_str().unwrap().to_owned();
            assert_eq!(hit["thread_scope"]["thread_id"], "fork");
            assert_eq!(
                hit["thread_scope"]["on_current_path"],
                matches!(id.as_str(), "shared" | "summary")
            );
            id
        })
        .collect::<Vec<_>>();
    keyword_ids.sort();
    assert_eq!(keyword_ids, ids);
    f.db.call(|db| {
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='fork'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.service.search_filtered(&query).await.unwrap_err().code,
        shellboy::protocol::ErrorCode::ThreadNotFound
    );
    f.close().await;
}
#[tokio::test]
async fn search_time_bounds_use_absolute_instants_and_current_source_witnesses() {
    let f = Fixture::new(Config::default()).await;
    for id in [
        "before",
        "at-from",
        "inside",
        "at-to",
        "stale",
        "changed-hash",
        "deleted",
    ] {
        f.node(id, "cat time").await;
    }
    f.db.call(|db| {
        db.execute_batch("UPDATE node SET created_at='2026-01-01T09:00:00+09:00'; UPDATE node SET created_at='2025-12-31T23:59:59.999999999Z' WHERE id='before'; UPDATE node SET created_at='2026-01-01T00:00:00.999999999Z' WHERE id='inside'; UPDATE node SET created_at='2026-01-01T01:00:01+01:00' WHERE id='at-to';")?; Ok(())
    }).await.unwrap();
    f.service
        .enqueue_new(&[
            "before".into(),
            "at-from".into(),
            "inside".into(),
            "at-to".into(),
            "stale".into(),
            "changed-hash".into(),
            "deleted".into(),
        ])
        .await
        .unwrap();
    f.drain().await;
    f.db.call(|db| { db.execute_batch("UPDATE node SET payload_revision=payload_revision+1 WHERE id='stale'; UPDATE node SET body_hash='changed' WHERE id='changed-hash'; UPDATE node SET deleted_reason='removed' WHERE id='deleted';")?; Ok(()) }).await.unwrap();
    let mut query = search_args("cat", 50);
    query.from = Some("2026-01-01T00:00:00Z".into());
    query.to = Some("2026-01-01T00:00:01Z".into());
    let hits = f.service.search_filtered(&query).await.unwrap();
    assert!(hits.iter().all(|hit| hit.thread_scope.is_none()));
    let mut ids = hits.into_iter().map(|h| h.node_id).collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec!["at-from", "inside"]);
    f.close().await;
}

#[tokio::test]
async fn search_excludes_stale_raw_generation_encoding_and_sequence_before_ranking() {
    let f = Fixture::new(Config::default()).await;
    raw_source(&f, b"cat raw witness".to_vec()).await;
    f.service.enqueue_tool_raw("raw-node").await.unwrap();
    f.drain().await;
    let query = search_args("cat", 1);
    assert_eq!(f.service.search_filtered(&query).await.unwrap().len(), 1);
    // Deliberately bypass normal invalidation to prove that search itself checks
    // the indexed raw witness, rather than trusting vector rows still present.
    for change in [
        "retention_generation=retention_generation+1",
        "output_encoding='Shift_JIS'",
        "saved_sequence=saved_sequence+1",
    ] {
        let change = change.to_owned();
        let original=f.db.call(move|db| {
            let original=db.query_row("SELECT retention_generation,output_encoding,saved_sequence FROM shell_execution WHERE id='execution'",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?;
            db.execute(&format!("UPDATE shell_execution SET {change} WHERE id='execution'"),[])?; Ok(original)
        }).await.unwrap();
        assert!(f.service.search_filtered(&query).await.unwrap().is_empty());
        f.db.call(move|db| { db.execute("UPDATE shell_execution SET retention_generation=?1,output_encoding=?2,saved_sequence=?3 WHERE id='execution'",params![original.0,original.1,original.2])?;Ok(()) }).await.unwrap();
    }
    f.close().await;
}

#[tokio::test]
async fn search_node_dedup_grows_knn_and_crosses_vec0_k_ceiling_without_starvation() {
    let f = Fixture::new(Config::default()).await;
    f.node("many-chunks", "cat").await;
    f.node("distant-node", "dog").await;
    f.service
        .enqueue_new(&["many-chunks".into(), "distant-node".into()])
        .await
        .unwrap();
    f.drain().await;
    // Stress grouping independently of embedding requests: repeated vector rows
    // retain the original valid job/source witness and source interval.
    f.db.call(|db| {
        let (table,space,original):(String,String,i64)=db.query_row("SELECT s.vec_table_name,c.space_id,c.vec_row_id FROM embedding_chunk c JOIN index_space s ON s.id=c.space_id WHERE c.source_node_id='many-chunks'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let tx=db.transaction()?;
        for i in 0..4100 {
            tx.execute(&format!("INSERT INTO {table}(embedding) SELECT embedding FROM {table} WHERE rowid=?1"),[original])?;
            let row=tx.last_insert_rowid();
            tx.execute("INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id) SELECT ?1,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,?2 FROM embedding_chunk WHERE space_id=?3 AND vec_row_id=?4",params![format!("duplicate-{i}"),row,space,original])?;
        }
        tx.commit()?;Ok(())
    }).await.unwrap();
    let hits = f
        .service
        .search_filtered(&search_args("cat", 2))
        .await
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["many-chunks", "distant-node"]
    );
    assert_eq!(hits[0].spans.len(), 4101);
    assert!(hits[0].distance < hits[1].distance);
    f.close().await;
}

#[tokio::test]
async fn unknown_index_space_is_a_nonfatal_target_error_and_worker_remains_usable() {
    let f = Fixture::new(Config::default()).await;
    let error = f.service.select_space("missing-space").await.unwrap_err();
    assert_eq!(error.code, shellboy::protocol::ErrorCode::TargetUnavailable);
    assert!(f.proxy.requests.lock().unwrap().is_empty());
    f.node("after-rejection", "The database remains writable")
        .await;
    let count: i64 =
        f.db.call(|db| {
            Ok(db.query_row(
                "SELECT COUNT(*) FROM node WHERE id='after-rejection'",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    f.close().await;
}

#[tokio::test]
async fn billing_failure_pauses_before_next_input_and_explicit_retry_keeps_authorized_scope() {
    let f = Fixture::new(Config::default()).await;
    for node in ["first", "second"] {
        f.node(node, "cat").await;
        f.service.enqueue_new(&[node.into()]).await.unwrap();
    }
    f.node("unapproved", "old dog").await;
    f.proxy.push(Reply::Error(402, "payment_required"));
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    let status = f.service.status(args()).await.unwrap();
    assert!(status.paused, "HTTP 402 is a shared billing failure");
    assert_eq!((status.failed, status.pending), (1, 1));
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(f.proxy.count(), 1);
    f.service.retry(args()).await.unwrap();
    f.drain().await;
    assert_eq!(f.proxy.count(), 3);
    assert_eq!(f.service.status(args()).await.unwrap().done, 2);
    let count =
        f.db.call(|db| {
            Ok(db.query_row(
                "SELECT COUNT(*) FROM embedding_job WHERE source_node_id='unapproved'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 0);
    f.close().await;
}

#[tokio::test]
async fn repeated_identical_unknown_provider_or_protocol_failures_pause_without_auto_retry() {
    for protocol in [false, true] {
        let f = Fixture::new(Config::default()).await;
        for i in 0..4 {
            let node = format!("node{i}");
            f.node(&node, "cat").await;
            f.service.enqueue_new(&[node]).await.unwrap();
        }
        for i in 0..3 {
            f.proxy.push(if protocol {
                Reply::ProtocolError
            } else {
                Reply::Error(400, "unknown_failure")
            });
            assert!(
                f.service
                    .process_one(CancellationToken::new())
                    .await
                    .unwrap()
            );
            assert_eq!(
                f.service.status(args()).await.unwrap().paused,
                i == 2,
                "protocol={protocol}, terminal failure={i}"
            );
        }
        assert!(
            !f.service
                .process_one(CancellationToken::new())
                .await
                .unwrap()
        );
        let status = f.service.status(args()).await.unwrap();
        assert_eq!((status.failed, status.pending), (3, 1));
        assert_eq!(
            f.proxy.count(),
            3,
            "unknown errors are not automatic retries or splits"
        );
        f.service.retry(args()).await.unwrap();
        f.drain().await;
        assert_eq!(f.service.status(args()).await.unwrap().done, 4);
        assert_eq!(f.proxy.count(), 7);
        f.close().await;
    }
}

#[tokio::test]
async fn intervening_unknown_failure_and_success_break_exhausted_transient_streaks() {
    let mut config = Config::default();
    config.indexing.retries = 0;
    config.indexing.retry_delays_ms.clear();
    let f = Fixture::new(config).await;
    for i in 0..8 {
        let node = format!("node{i}");
        f.node(&node, "cat").await;
        f.service.enqueue_new(&[node]).await.unwrap();
    }
    for reply in [
        Reply::Error(503, "server_error"),
        Reply::Error(400, "unknown_failure"),
        Reply::Error(503, "server_error"),
        Reply::Error(503, "server_error"),
        Reply::Embed(3),
        Reply::Error(503, "server_error"),
        Reply::Error(503, "server_error"),
    ] {
        let before = f.proxy.count();
        f.proxy.push(reply);
        for _ in 0..100 {
            assert!(
                f.service
                    .process_one(CancellationToken::new())
                    .await
                    .unwrap()
            );
            if f.proxy.count() > before {
                break;
            }
        }
        assert_eq!(f.proxy.count(), before + 1);
        assert!(
            !f.service.status(args()).await.unwrap().paused,
            "only consecutive exhausted jobs count"
        );
    }
    let status = f.service.status(args()).await.unwrap();
    assert_eq!((status.failed, status.done, status.pending), (6, 1, 1));
    assert_eq!(f.proxy.count(), 7);
    f.close().await;
}

#[tokio::test]
async fn a_different_unknown_cause_breaks_the_same_cause_streak() {
    let f = Fixture::new(Config::default()).await;
    for i in 0..6 {
        let node = format!("node{i}");
        f.node(&node, "cat").await;
        f.service.enqueue_new(&[node]).await.unwrap();
    }
    for code in [
        "unknown_a",
        "unknown_a",
        "unknown_b",
        "unknown_a",
        "unknown_a",
    ] {
        f.proxy.push(Reply::Error(400, code));
        assert!(
            f.service
                .process_one(CancellationToken::new())
                .await
                .unwrap()
        );
        assert!(!f.service.status(args()).await.unwrap().paused);
    }
    f.proxy.push(Reply::Error(400, "unknown_a"));
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert!(f.service.status(args()).await.unwrap().paused);
    assert_eq!(f.proxy.count(), 6);
    f.close().await;
}

#[tokio::test]
async fn explicit_length_split_breaks_the_exhausted_transient_streak() {
    let mut config = Config::default();
    config.indexing.retries = 0;
    config.indexing.retry_delays_ms.clear();
    let f = Fixture::new(config).await;
    for i in 0..3 {
        let node = format!("node{i}");
        f.node(&node, "cat body long enough").await;
        f.service.enqueue_new(&[node]).await.unwrap();
    }
    for reply in [
        Reply::Error(503, "server_error"),
        Reply::Error(503, "server_error"),
        Reply::Error(400, "input_too_long"),
        Reply::Error(503, "server_error"),
    ] {
        f.proxy.push(reply);
        assert!(
            f.service
                .process_one(CancellationToken::new())
                .await
                .unwrap()
        );
        assert!(!f.service.status(args()).await.unwrap().paused);
    }
    assert_eq!(f.proxy.count(), 4);
    f.close().await;
}

fn configured_service(f: &Fixture, split_chars: usize) -> Arc<IndexService> {
    let mut config = Config::default();
    config.indexing.split_chars = split_chars;
    config.indexing.retries = 0;
    config.indexing.retry_delays_ms.clear();
    config.connections.insert(
        "embed".into(),
        Connection {
            base_url: f.proxy.url.clone(),
            ..Connection::default()
        },
    );
    config.embedding.connection = Some("embed".into());
    config.embedding.model = Some("embedding-fixture".into());
    IndexService::new(f.db.clone(), Arc::new(config))
}

async fn drain_service(service: &IndexService) {
    for _ in 0..1000 {
        if !service.process_one(CancellationToken::new()).await.unwrap() {
            return;
        }
    }
    panic!("index service did not settle");
}

#[tokio::test]
async fn changing_split_chars_changes_space_and_fee_preview_without_implicit_backfill() {
    let mut config = Config::default();
    config.indexing.split_chars = 8;
    let f = Fixture::new(config).await;
    f.node("source", "abcdefghijklmnopqrstuvwx").await;
    f.node("not-approved", "another historical body").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    f.drain().await;
    let old = f.service.status(args()).await.unwrap().space_id.unwrap();
    assert_eq!(f.proxy.count(), 3);
    let changed = configured_service(&f, 12);
    assert!(!changed.process_one(CancellationToken::new()).await.unwrap());
    assert_eq!(
        f.proxy.count(),
        3,
        "configuration changes do not authorize historical work"
    );
    let selected = IndexArgs {
        source_ids: vec!["source".into()],
        ..args()
    };
    let preview = changed.preview(selected.clone()).await.unwrap();
    assert_eq!(
        (preview.segments, preview.already_indexed),
        (2, 0),
        "old ranges cannot fund a different segmentation identity"
    );
    assert_eq!(f.proxy.count(), 3);
    let queued = changed.backfill(selected.clone()).await.unwrap();
    assert_eq!((queued.segments, queued.already_indexed), (2, 0));
    drain_service(&changed).await;
    let current = changed.status(args()).await.unwrap().space_id.unwrap();
    assert_ne!(old, current);
    assert_eq!(f.proxy.count(), 5);
    let old_copy = old.clone();
    let current_copy = current.clone();
    f.db.call(move |db| {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM embedding_chunk WHERE space_id=?1",
                [old_copy],
                |r| r.get::<_, i64>(0)
            )?,
            3
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM embedding_chunk WHERE space_id=?1",
                [current_copy],
                |r| r.get::<_, i64>(0)
            )?,
            2
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM embedding_job WHERE source_node_id='not-approved'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    let preview = changed.preview(selected.clone()).await.unwrap();
    assert_eq!((preview.segments, preview.already_indexed), (2, 2));
    let repeated = changed.backfill(selected).await.unwrap();
    assert_eq!((repeated.segments, repeated.already_indexed), (0, 2));
    drain_service(&changed).await;
    assert_eq!(f.proxy.count(), 5);
    assert!(changed.select_space(&old).await.is_err());
    let original = configured_service(&f, 8);
    original.select_space(&old).await.unwrap();
    assert_eq!(
        original.status(args()).await.unwrap().space_id.as_deref(),
        Some(old.as_str())
    );
    f.close().await;
}

#[tokio::test]
async fn model_switch_selects_only_successful_new_work_and_keeps_old_space_restorable() {
    let f = Fixture::new(Config::default()).await;
    f.node("old-model-source", "old model history").await;
    f.service
        .enqueue_new(&["old-model-source".into()])
        .await
        .unwrap();
    f.drain().await;
    let old = f.service.status(args()).await.unwrap().space_id.unwrap();
    f.node(
        "not-approved",
        "historical input not approved for the new model",
    )
    .await;
    let mut config = Config::default();
    config.connections.insert(
        "embed".into(),
        Connection {
            base_url: f.proxy.url.clone(),
            ..Connection::default()
        },
    );
    config.embedding.connection = Some("embed".into());
    config.embedding.model = Some("changed-model".into());
    let changed = IndexService::new(f.db.clone(), Arc::new(config));
    assert!(!changed.process_one(CancellationToken::new()).await.unwrap());
    assert_eq!(
        f.proxy.count(),
        1,
        "changing configuration grants no historical work"
    );
    f.node("new-model-source", "new model conversation").await;
    f.proxy.push(Reply::ProtocolError);
    changed
        .enqueue_new(&["new-model-source".into()])
        .await
        .unwrap();
    drain_service(&changed).await;
    assert_eq!(
        changed.status(args()).await.unwrap().space_id.as_deref(),
        Some(old.as_str())
    );
    f.db.call(|db| {
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM index_space", [], |r| r
                .get::<_, i64>(0))?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    let old_hits = f.service.search("old", 10).await.unwrap();
    assert_eq!(old_hits.len(), 1);
    assert_eq!(old_hits[0].node_id, "old-model-source");
    changed.retry(args()).await.unwrap();
    drain_service(&changed).await;
    let current = changed.status(args()).await.unwrap().space_id.unwrap();
    assert_ne!(current, old);
    let hits = changed.search("new", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_id, "new-model-source");
    let old_copy = old.clone();
    f.db.call(move |db| {
        assert_eq!(
            db.query_row(
                "SELECT state FROM index_space WHERE id=?1",
                [old_copy],
                |r| r.get::<_, String>(0)
            )?,
            "inactive"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM embedding_chunk", [], |r| r
                .get::<_, i64>(0))?,
            2
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM embedding_job WHERE source_node_id='not-approved'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'index.rebuild.%'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    assert!(changed.select_space(&old).await.is_err());
    f.service.select_space(&old).await.unwrap();
    let restored = f.service.search("old", 10).await.unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].node_id, "old-model-source");
    let requests = f.proxy.requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        6,
        "two successful source embeddings, one failed attempt, and three explicit queries only"
    );
    assert_eq!(requests[1]["model"], "changed-model");
    assert_eq!(requests[3]["model"], "changed-model");
    f.close().await;
}

#[tokio::test]
async fn deleted_thread_exclusive_nodes_disappear_before_gc_while_live_roots_and_unknown_owners_stay()
 {
    let f = Fixture::new(Config::default()).await;
    let ids = [
        "only-path",
        "only-turn",
        "shared",
        "source",
        "nested",
        "summary",
        "live-unselected",
        "accepted",
        "pinned",
        "unknown",
    ];
    for id in ids {
        if matches!(id, "summary" | "nested") {
            f.payload(id,if id=="summary" {"summary"}else{"spill"},json!({"item":{"role":"developer","content":[{"type":"input_text","text":"cat retained history"}]}})).await;
        } else {
            f.node(id, "cat retained history").await;
        }
    }
    f.service
        .enqueue_new(&ids.map(str::to_owned))
        .await
        .unwrap();
    f.drain().await;
    let indexed = f.proxy.count();
    f.db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('deleted','/','/a','p','m','now'),('live','/','/a','p','m','now');
        INSERT INTO run(id,thread_id,state,created_at)VALUES('dead-run','deleted','completed','now'),('live-run','live','completed','now');
        INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('dead-turn','deleted','dead-run','completed','now'),('live-turn','live','live-run','completed','now');
        UPDATE node SET turn_id='dead-turn' WHERE id='only-turn';
        UPDATE node SET turn_id='live-turn' WHERE id='live-unselected';
        INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('deleted',0,'only-path'),('deleted',1,'shared'),('deleted',2,'source'),('deleted',3,'accepted'),('deleted',4,'pinned'),('live',0,'shared'),('live',1,'summary');
        INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES('replacement','summary','key','1','1','completed','now'),('nested-replacement','nested','nested-key','1','1','completed','now');
        INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES('replacement',0,'nested',0),('nested-replacement',0,'source',0);
        INSERT INTO receipt(id,client_operation_id,sender_json,message_node_id,state,target_thread_id,created_at)VALUES('receipt','op','null','accepted','accepted','deleted','now');
        INSERT INTO runtime_pin(id,resource_kind,resource_id,instance_id,generation,created_at)VALUES('pin','node','pinned','fixture',0,'now');")?;
        Ok(())
    }).await.unwrap();
    assert_eq!(
        f.service.search_detailed("cat", 50).await.unwrap().len(),
        ids.len()
    );
    f.db.call(|db| {
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='deleted'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    let mut found = f
        .service
        .search_detailed("cat", 50)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.node_id)
        .collect::<Vec<_>>();
    found.sort();
    assert_eq!(
        found,
        vec![
            "accepted",
            "live-unselected",
            "nested",
            "pinned",
            "shared",
            "source",
            "summary",
            "unknown"
        ]
    );
    assert_eq!(
        f.proxy.count(),
        indexed + 2,
        "only query embeddings are billed; deleting does not reindex"
    );
    f.db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            10
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM embedding_chunk", [], |r| r
                .get::<_, i64>(0))?,
            10
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.call(|db| {
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='live'", [])?;
        db.execute(
            "UPDATE receipt SET state='cancelled' WHERE id='receipt'",
            [],
        )?;
        db.execute("DELETE FROM runtime_pin WHERE id='pin'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    let found = f.service.search_detailed("cat", 50).await.unwrap();
    assert_eq!(
        found
            .iter()
            .map(|hit| hit.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["unknown"]
    );
    f.close().await;
}

#[tokio::test]
async fn real_thread_delete_hides_path_only_sources_until_gc_reclaims_payload_and_ownership_rows() {
    let f = Fixture::new(Config::default()).await;
    f.node("original", "cat original").await;
    f.payload(
        "summary",
        "summary",
        json!({"item":{"role":"developer","content":[{"type":"input_text","text":"cat summary"}]}}),
    )
    .await;
    f.service
        .enqueue_new(&["original".into(), "summary".into()])
        .await
        .unwrap();
    f.drain().await;
    f.db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('deleted','/','/a','p','m','now'); INSERT INTO node(id,kind,created_at,deleted_reason)VALUES('old-tombstone','user','now','old collected'); INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('deleted',0,'original'),('deleted',1,'summary'),('deleted',2,'old-tombstone'); INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES('r','summary','k','1','1','completed','now'); INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES('r',0,'original',0); INSERT INTO thread_replacement(thread_id,replacement_id)VALUES('deleted','r');")?;
        Ok(())
    }).await.unwrap();
    assert_eq!(f.service.search_detailed("cat", 50).await.unwrap().len(), 2);
    let store = shellboy::control::db::Store::new(
        f.db.clone(),
        Arc::new(Config::default()),
        f._temp.path().into(),
        "delete-fixture".into(),
    );
    store
        .execute(
            shellboy::protocol::Request::new(
                "delete-fixture:delete",
                shellboy::protocol::Command::ThreadDelete(shellboy::protocol::ThreadDeleteArgs {
                    thread_id: "deleted".into(),
                    descendants: false,
                }),
            ),
            None,
        )
        .await
        .unwrap();
    assert!(
        f.service
            .search_detailed("cat", 50)
            .await
            .unwrap()
            .is_empty()
    );
    let preview = f
        .service
        .preview(IndexArgs {
            thread_ids: vec!["deleted".into()],
            ..args()
        })
        .await
        .unwrap();
    assert_eq!(
        preview.sources, 0,
        "preserved deleted ownership is not new index authorization"
    );
    f.db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM thread_path WHERE thread_id='deleted'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            3
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM thread_replacement WHERE thread_id='deleted'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            2
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.call(|db| {
        db.execute_batch("CREATE TRIGGER fail_owner_cleanup BEFORE DELETE ON thread_replacement WHEN OLD.thread_id='deleted' BEGIN SELECT RAISE(ABORT,'owner cleanup failure'); END;")?;Ok(())
    }).await.unwrap();
    assert!(
        f.raw_gc()
            .collect(
                shellboy::protocol::StorageCategory::History,
                chrono::Utc::now()
            )
            .await
            .is_err()
    );
    f.db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            2
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM thread_path", [], |r| r
                .get::<_, i64>(0))?,
            3
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM thread_replacement", [], |r| r
                .get::<_, i64>(0))?,
            1
        );
        db.execute("DROP TRIGGER fail_owner_cleanup", [])?;
        Ok(())
    })
    .await
    .unwrap();
    f.raw_gc()
        .collect(
            shellboy::protocol::StorageCategory::History,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    f.db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM thread_path", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM thread_replacement", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    f.close().await;
}

#[tokio::test]
async fn paused_or_inactive_inflight_space_keeps_fixed_input_for_explicit_same_space_retry() {
    for inactive in [false, true] {
        let f = Fixture::new(Config::default()).await;
        f.node("seed", "cat seed").await;
        f.service.enqueue_new(&["seed".into()]).await.unwrap();
        f.drain().await;
        let old = f.service.status(args()).await.unwrap().space_id.unwrap();
        let alternate = if inactive {
            f.service
                .rebuild(IndexArgs {
                    source_ids: vec!["seed".into()],
                    ..args()
                })
                .await
                .unwrap();
            f.drain().await;
            let newer = f.service.status(args()).await.unwrap().space_id.unwrap();
            f.service.select_space(&old).await.unwrap();
            Some(newer)
        } else {
            None
        };
        f.node("target", "cat fixed retry input").await;
        f.service.enqueue_new(&["target".into()]).await.unwrap();
        let before = f.proxy.count();
        let expected=f.db.call(|db|Ok(db.query_row("SELECT value FROM app_meta WHERE key=(SELECT 'index.input.'||id FROM embedding_job WHERE source_node_id='target')",[],|r|r.get::<_,String>(0))?)).await.unwrap();
        let (entered, ready) = oneshot::channel();
        let (release, hold) = oneshot::channel();
        f.proxy.push(Reply::Wait(entered, hold, 3));
        let service = f.service.clone();
        let running =
            tokio::spawn(async move { service.process_one(CancellationToken::new()).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        if let Some(newer) = &alternate {
            f.service.select_space(newer).await.unwrap();
        } else {
            let old = old.clone();
            f.db.call(move |db| {
                db.execute("UPDATE index_space SET state='paused' WHERE id=?1", [old])?;
                Ok(())
            })
            .await
            .unwrap();
        }
        release.send(()).unwrap();
        assert!(running.await.unwrap().unwrap());
        f.db.call(move|db| {
            assert_eq!(db.query_row("SELECT state FROM embedding_job WHERE source_node_id='target'",[],|r|r.get::<_,String>(0))?,"failed");
            assert_eq!(db.query_row("SELECT json_extract(error_json,'$.kind') FROM embedding_job WHERE source_node_id='target'",[],|r|r.get::<_,String>(0))?,"space_unavailable");
            assert_eq!(db.query_row("SELECT COUNT(*) FROM embedding_chunk WHERE source_node_id='target'",[],|r|r.get::<_,i64>(0))?,0);
            assert_eq!(db.query_row("SELECT value FROM app_meta WHERE key=(SELECT 'index.input.'||id FROM embedding_job WHERE source_node_id='target')",[],|r|r.get::<_,String>(0))?,expected,"space changes do not invalidate authorized text");
            Ok(())
        }).await.unwrap();
        assert!(
            !f.service
                .process_one(CancellationToken::new())
                .await
                .unwrap()
        );
        assert_eq!(f.proxy.count(), before + 1, "no automatic rebilling");
        if inactive {
            assert!(
                f.service
                    .retry(IndexArgs {
                        space_id: Some(old.clone()),
                        ..args()
                    })
                    .await
                    .is_err()
            );
            f.service.select_space(&old).await.unwrap();
        }
        f.service
            .retry(IndexArgs {
                space_id: Some(old.clone()),
                source_ids: vec!["target".into()],
                ..args()
            })
            .await
            .unwrap();
        f.drain().await;
        assert_eq!(f.proxy.count(), before + 2);
        assert_eq!(
            f.service.status(args()).await.unwrap().space_id.as_deref(),
            Some(old.as_str())
        );
        f.db.call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT state FROM embedding_job WHERE source_node_id='target'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "done"
            );
            Ok(())
        })
        .await
        .unwrap();
        f.close().await;
    }
}

#[tokio::test]
async fn paused_candidate_inflight_result_keeps_old_active_until_explicit_retry_finishes() {
    let mut config = Config::default();
    config.indexing.split_chars = 8;
    let f = Fixture::new(config).await;
    f.node("seed", "cat").await;
    f.service.enqueue_new(&["seed".into()]).await.unwrap();
    f.drain().await;
    let old = f.service.status(args()).await.unwrap().space_id.unwrap();
    f.node("rebuild", "abcdefghijklmnop").await;
    f.service
        .rebuild(IndexArgs {
            source_ids: vec!["rebuild".into()],
            ..args()
        })
        .await
        .unwrap();
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    let candidate =
        f.db.call(|db| {
            Ok(db.query_row(
                "SELECT id FROM index_space WHERE state='candidate'",
                [],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    let before = f.proxy.count();
    let (entered, ready) = oneshot::channel();
    let (release, hold) = oneshot::channel();
    f.proxy.push(Reply::Wait(entered, hold, 3));
    let service = f.service.clone();
    let running = tokio::spawn(async move { drain_service(&service).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), ready)
        .await
        .unwrap()
        .unwrap();
    let paused = candidate.clone();
    f.db.call(move |db| {
        db.execute(
            "UPDATE index_space SET state='candidate_paused' WHERE id=?1",
            [paused],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    release.send(()).unwrap();
    running.await.unwrap();
    let old_copy = old.clone();
    f.db.call(move|db| {
        assert_eq!(db.query_row("SELECT value FROM app_meta WHERE key='index.active_space'",[],|r|r.get::<_,String>(0))?,old_copy);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM embedding_chunk WHERE source_node_id='rebuild'",[],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM embedding_job j JOIN app_meta m ON m.key='index.input.'||j.id WHERE j.source_node_id='rebuild' AND j.state='failed'",[],|r|r.get::<_,i64>(0))?,1);
        Ok(())
    }).await.unwrap();
    assert_eq!(f.proxy.count(), before + 1);
    f.service
        .retry(IndexArgs {
            space_id: Some(candidate.clone()),
            ..args()
        })
        .await
        .unwrap();
    f.drain().await;
    assert_eq!(f.proxy.count(), before + 2);
    assert_eq!(
        f.service.status(args()).await.unwrap().space_id.as_deref(),
        Some(candidate.as_str())
    );
    f.db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM embedding_chunk WHERE source_node_id='rebuild'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            2
        );
        Ok(())
    })
    .await
    .unwrap();
    f.close().await;
}

#[tokio::test]
async fn changing_uncoded_400_details_do_not_bypass_the_terminal_failure_breaker() {
    let mut config = Config::default();
    config.indexing.retries = 0;
    config.indexing.retry_delays_ms.clear();
    let f = Fixture::new(config).await;
    for i in 0..4 {
        let node = format!("uncoded-{i}");
        f.node(&node, "cat body").await;
        f.service.enqueue_new(&[node]).await.unwrap();
    }
    for i in 0..3 {
        f.proxy.push(Reply::UncodedError(format!(
            "request {i}: provider rejected input {i}"
        )));
        assert!(
            f.service
                .process_one(CancellationToken::new())
                .await
                .unwrap()
        );
        assert_eq!(f.service.status(args()).await.unwrap().paused, i == 2);
    }
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(
        f.proxy.count(),
        3,
        "remaining authorized source must not incur another request while paused"
    );
    f.close().await;
}

#[tokio::test]
async fn query_identity_survives_split_changes_and_never_silently_rebinds_endpoints() {
    let mut config = Config::default();
    config.indexing.split_chars = 8;
    let f = Fixture::new(config).await;
    f.node("query-source", "cat source").await;
    f.service
        .enqueue_new(&["query-source".into()])
        .await
        .unwrap();
    f.drain().await;
    let space = f.service.status(args()).await.unwrap().space_id.unwrap();
    let marker_key = format!("index.query_profile.{space}");
    let marker_copy = marker_key.clone();
    let marker =
        f.db.call(move |db| {
            Ok(db.query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [marker_copy],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(marker.len(), 64);
    assert!(marker.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let changed = configured_service(&f, 12);
    assert!(!changed.search("cat", 10).await.unwrap().is_empty());
    assert!(
        changed.select_space(&space).await.is_err(),
        "select still requires matching split configuration"
    );
    f.service.select_space(&space).await.unwrap();
    assert!(!changed.search("cat", 10).await.unwrap().is_empty());
    let other = Proxy::new().await;
    let mut config = Config::default();
    config.indexing.split_chars = 8;
    config.connections.insert(
        "embed".into(),
        Connection {
            base_url: other.url.clone(),
            ..Default::default()
        },
    );
    config.embedding.connection = Some("embed".into());
    config.embedding.model = Some("embedding-fixture".into());
    let endpoint_changed = IndexService::new(f.db.clone(), Arc::new(config));
    assert!(endpoint_changed.search("cat", 10).await.is_err());
    assert!(endpoint_changed.select_space(&space).await.is_err());
    assert_eq!(
        other.count(),
        0,
        "endpoint changes require an explicit new index, not query rebinding"
    );
    let marker_copy = marker_key.clone();
    f.db.call(move |db| {
        assert_eq!(
            db.query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [&marker_copy],
                |r| r.get::<_, String>(0)
            )?,
            marker
        );
        db.execute(
            "UPDATE app_meta SET value='malformed' WHERE key=?1",
            [marker_copy],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let requests = f.proxy.count();
    assert!(f.service.search("cat", 10).await.is_err());
    assert!(f.service.select_space(&space).await.is_err());
    assert_eq!(
        f.proxy.count(),
        requests,
        "corrupt marker must not fall back to matching splitter"
    );
    other.close().await;
    f.close().await;
}

#[tokio::test]
async fn missing_query_marker_is_rejected_without_http_or_read_repair() {
    let f = Fixture::new(Config::default()).await;
    f.node("query-source", "cat source").await;
    f.service
        .enqueue_new(&["query-source".into()])
        .await
        .unwrap();
    f.drain().await;
    let space = f.service.status(args()).await.unwrap().space_id.unwrap();
    f.db.call(|db| {
        db.execute(
            "DELETE FROM app_meta WHERE key LIKE 'index.query_profile.%'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let requests = f.proxy.count();
    for error in [
        f.service.search("cat", 10).await.unwrap_err(),
        f.service.select_space(&space).await.unwrap_err(),
    ] {
        assert_eq!(error.code, shellboy::protocol::ErrorCode::InvalidState);
        assert!(error.message.contains("explicitly rebuild"));
    }
    assert_eq!(
        f.proxy.count(),
        requests,
        "verification failure must precede paid HTTP"
    );
    f.db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'index.query_profile.%'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    f.close().await;
}

async fn expire_embedding(f: &Fixture) {
    let (entered, _started) = oneshot::channel();
    let (release, held) = oneshot::channel();
    f.proxy.push(Reply::Wait(entered, held, 3));
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            f.service.process_one(CancellationToken::new()),
        )
        .await
        .expect("embedding deadline must release the worker slot")
        .unwrap()
    );
    let _ = release.send(());
}

#[tokio::test]
async fn stalled_embeddings_retry_then_pause_and_manual_retry_restores_pending_work() {
    let mut config = Config::default();
    config.indexing.retries = 1;
    config.indexing.retry_delays_ms = vec![60_000];
    config.indexing.common_failure_pause_count = 1;
    let f = Fixture::with_request_timeout(config, 1_000).await;
    f.node("stalled", "cat timeout fixture").await;
    f.service.enqueue_new(&["stalled".into()]).await.unwrap();
    expire_embedding(&f).await;
    let first = f.service.status(args()).await.unwrap();
    assert!(!first.paused);
    assert_eq!(
        first.failed, 0,
        "first timeout schedules the permitted retry"
    );
    let timeouts = f.db.call(|db| {
        let errors: i64 = db.query_row("SELECT count(*) FROM app_meta WHERE key LIKE 'index.bootstrap.%' AND json_extract(value,'$.error.kind')='timeout' AND json_extract(value,'$.next_at') IS NOT NULL", [], |row| row.get(0))?;
        db.execute("UPDATE app_meta SET value=json_set(value,'$.next_at','2000-01-01T00:00:00Z') WHERE key LIKE 'index.bootstrap.%' AND json_extract(value,'$.error.kind')='timeout'", [])?;
        Ok(errors)
    }).await.unwrap();
    assert_eq!(timeouts, 1);
    expire_embedding(&f).await;
    f.node("later", "cat later fixture").await;
    f.service.enqueue_new(&["later".into()]).await.unwrap();
    let status = f.service.status(args()).await.unwrap();
    assert!(status.paused);
    assert_eq!(status.failed, 1);
    assert_eq!(status.pending, 1);
    assert!(
        !f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(
        f.proxy.count(),
        2,
        "pause prevents the next paid HTTP input"
    );
    f.service.retry(args()).await.unwrap();
    f.drain().await;
    assert_eq!(f.service.status(args()).await.unwrap().done, 2);
    assert_eq!(f.proxy.count(), 4);
    f.close().await;
}

#[tokio::test]
async fn query_embedding_caller_disconnect_releases_stalled_http_and_allows_next_query() {
    let f = Fixture::new(Config::default()).await;
    f.node("source", "cat search fixture").await;
    f.service.enqueue_new(&["source".into()]).await.unwrap();
    f.drain().await;
    let before = f.service.status(args()).await.unwrap();
    {
        let (entered, started) = oneshot::channel();
        let (release, held) = oneshot::channel();
        f.proxy.push(Reply::Wait(entered, held, 3));
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let service = f.service.clone();
        let query = tokio::spawn(async move {
            service
                .search_filtered_cancellable(
                    &shellboy::protocol::HistorySearchArgs {
                        query: "cat".into(),
                        ..Default::default()
                    },
                    token,
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), started)
            .await
            .unwrap()
            .unwrap();
        cancel.cancel();
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), query)
            .await
            .expect("cancellation must not wait for the HTTP deadline")
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, shellboy::protocol::ErrorCode::InvalidState);
        let _ = release.send(());
        let after = f.service.status(args()).await.unwrap();
        assert_eq!(after.done, before.done);
        assert_eq!(after.failed, before.failed);
        assert_eq!(after.paused, before.paused);
    }
    assert!(!f.service.search("cat", 10).await.unwrap().is_empty());
    assert_eq!(f.proxy.count(), 3);
    f.close().await;
}

#[tokio::test]
async fn old_integer_reset_migrates_before_real_workers_and_exhausted_failures_trip_breaker() {
    let mut config = Config::default();
    config.indexing.retries = 0;
    config.indexing.retry_delays_ms.clear();
    let mut f = Fixture::new(config).await;
    f.node("seed", "cat seed").await;
    f.service.enqueue_new(&["seed".into()]).await.unwrap();
    f.drain().await;
    for i in 0..4 {
        let id = format!("failed-{i}");
        f.node(&id, "dog").await;
        f.service.enqueue_new(&[id]).await.unwrap();
    }
    let path = f._temp.path().join("db.sqlite");
    f.db.shutdown().await.unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE INDEX receipt_turn_source ON receipt(turn_id,target_thread_id,message_node_id); DELETE FROM schema_migration WHERE version=11; PRAGMA user_version=10; INSERT OR REPLACE INTO app_meta(key,value) SELECT 'index.common_failures.'||value,'0' FROM app_meta WHERE key GLOB 'index.query_profile.*';").unwrap();
    db.close().unwrap();
    f.db = DbWorker::open(&path).await.unwrap();
    f.service = IndexService::new(f.db.clone(), f.config.clone());
    for _ in 0..3 {
        f.proxy.push(Reply::Error(503, "server_error"));
    }
    let workers = f.service.start().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status = f.service.status(args()).await.unwrap();
            if status.paused {
                assert_eq!((status.done, status.failed, status.pending), (1, 3, 1));
                assert!(status.reason.unwrap().contains("server_error"));
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    workers.shutdown().await;
    assert_eq!(
        f.proxy.count(),
        4,
        "one seed plus exactly three exhausted terminal failures"
    );
    f.db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM embedding_job WHERE state='running'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        let value: String = db.query_row(
            "SELECT value FROM app_meta WHERE key GLOB 'index.common_failures.*'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(serde_json::from_str::<Value>(&value).unwrap()["count"], 3);
        Ok(())
    })
    .await
    .unwrap();
    f.close().await;
}

#[tokio::test]
async fn malformed_current_counter_preserves_provider_failure_and_pauses_real_worker() {
    let f = Fixture::new(Config::default()).await;
    f.node("seed", "cat seed").await;
    f.service.enqueue_new(&["seed".into()]).await.unwrap();
    f.drain().await;
    f.db.call(|db| {
        db.execute("INSERT OR REPLACE INTO app_meta(key,value) SELECT 'index.common_failures.'||value,'{\"cause\":\"bad\",\"count\":\"broken\"}' FROM app_meta WHERE key GLOB 'index.query_profile.*'",[])?;
        Ok(())
    }).await.unwrap();
    for i in 0..2 {
        let id = format!("pending-{i}");
        f.node(&id, "dog").await;
        f.service.enqueue_new(&[id]).await.unwrap();
    }
    f.proxy.push(Reply::Error(400, "unknown_failure"));
    let workers = f.service.start().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status = f.service.status(args()).await.unwrap();
            if status.paused {
                assert_eq!((status.done, status.failed, status.pending), (1, 1, 1));
                let reason = status.reason.unwrap();
                assert!(reason.contains("index_failure_adoption"), "{reason}");
                assert!(reason.contains("unknown_failure"), "{reason}");
                assert!(reason.contains("Stored index job is malformed"), "{reason}");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    workers.shutdown().await;
    assert_eq!(
        f.proxy.count(),
        2,
        "the paused worker must not request the second input"
    );
    f.db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM embedding_job WHERE state='running'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        let error: String = db.query_row(
            "SELECT error_json FROM embedding_job WHERE state='failed'",
            [],
            |r| r.get(0),
        )?;
        assert!(error.contains("unknown_failure"));
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM app_meta WHERE key GLOB 'index.input.*'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            2,
            "retain both fixed inputs for explicit retry"
        );
        Ok(())
    })
    .await
    .unwrap();
    f.service.retry(args()).await.unwrap();
    f.drain().await;
    let status = f.service.status(args()).await.unwrap();
    assert!(!status.paused);
    assert_eq!((status.done, status.failed, status.pending), (3, 0, 0));
    f.close().await;
}
