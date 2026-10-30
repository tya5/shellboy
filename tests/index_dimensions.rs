//! Explicit embedding dimensions through real sqlite-vec and local HTTP only.
use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shellboy::{
    app::DataOwner,
    config::{Config, Connection},
    control::Control,
    index::IndexService,
    protocol::{Command, ErrorCode, IndexArgs, IndexProbeArgs, Request},
    storage::DbWorker,
};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

struct Hold {
    entered: oneshot::Sender<Value>,
    release: oneshot::Receiver<()>,
}
#[derive(Default)]
struct Rules {
    reject: HashSet<usize>,
    wrong_length: HashSet<usize>,
    transient_once: HashSet<usize>,
    hold: Option<Hold>,
}
struct Proxy {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    rules: Arc<Mutex<Rules>>,
    stop: CancellationToken,
    task: JoinHandle<()>,
}
impl Proxy {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let rules = Arc::new(Mutex::new(Rules::default()));
        let responses = rules.clone();
        let stop = CancellationToken::new();
        let cancel = stop.clone();
        let task = tokio::spawn(async move {
            let mut handlers = JoinSet::new();
            loop {
                tokio::select! {
                    _=cancel.cancelled()=>break,
                    Some(result)=handlers.join_next(), if !handlers.is_empty()=>{result.unwrap();},
                    socket=listener.accept()=> {
                        let (socket,_)=socket.unwrap();
                        handlers.spawn(serve(socket,captured.clone(),responses.clone(),cancel.clone()));
                    }
                }
            }
            handlers.shutdown().await;
        });
        Self {
            url,
            requests,
            rules,
            stop,
            task,
        }
    }
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
    fn hold_next_explicit(&self) -> (oneshot::Receiver<Value>, oneshot::Sender<()>) {
        let (entered, receive) = oneshot::channel();
        let (release, released) = oneshot::channel();
        self.rules.lock().unwrap().hold = Some(Hold {
            entered,
            release: released,
        });
        (receive, release)
    }
    async fn close(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
}
async fn serve(
    mut socket: TcpStream,
    requests: Arc<Mutex<Vec<Value>>>,
    rules: Arc<Mutex<Rules>>,
    stop: CancellationToken,
) {
    let mut bytes = Vec::new();
    let request = loop {
        let mut chunk = [0; 8192];
        let n = tokio::select! {_=stop.cancelled()=>return, result=socket.read(&mut chunk)=>result.unwrap()};
        if n == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let length = String::from_utf8_lossy(&bytes[..at])
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            if bytes.len() >= at + 4 + length {
                break serde_json::from_slice::<Value>(&bytes[at + 4..at + 4 + length]).unwrap();
            }
        }
    };
    requests.lock().unwrap().push(request.clone());
    let requested = request
        .get("dimensions")
        .map(|n| n.as_u64().unwrap() as usize);
    let (dimension, reject, transient, hold) = {
        let mut rules = rules.lock().unwrap();
        let reject = requested.is_some_and(|n| rules.reject.contains(&n));
        let dimension = requested.map_or(2, |n| {
            if rules.wrong_length.contains(&n) {
                n + 1
            } else {
                n
            }
        });
        let hold = if requested.is_some() {
            rules.hold.take()
        } else {
            None
        };
        let transient = requested.is_some_and(|n| rules.transient_once.remove(&n));
        (dimension, reject, transient, hold)
    };
    if let Some(hold) = hold {
        let _ = hold.entered.send(request.clone());
        tokio::select! {_=stop.cancelled()=>return, _=hold.release=>{}}
    }
    let (status, body) = if transient {
        (
            500,
            json!({"error":{"code":"temporary_failure","message":"synthetic transient failure"}}),
        )
    } else if reject {
        (
            400,
            json!({"error":{"code":"unsupported_dimensions","message":"synthetic dimension refusal"}}),
        )
    } else {
        let data = request["input"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .rev()
            .map(|(index, _)| json!({"index":index,"embedding":vec![1.0;dimension]}))
            .collect::<Vec<_>>();
        (200, json!({"data":data}))
    };
    let body = body.to_string();
    let _=socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
}
struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    db: DbWorker,
    service: Arc<IndexService>,
    config: Arc<Config>,
    proxy: Proxy,
}
impl Fixture {
    async fn new() -> Self {
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
        config.embedding.model = Some("synthetic-dimensions".into());
        config.embedding.batch_max_inputs = 8;
        config.embedding.batch_max_chars = 512;
        config.indexing.split_chars = 64;
        // First-dimension refusal must stop independently of an ordinary retry budget.
        config.indexing.retries = 5;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("index.db");
        let db = DbWorker::open(&path).await.unwrap();
        let config = Arc::new(config);
        let service = IndexService::new(db.clone(), config.clone());
        let f = Self {
            _root: root,
            path,
            db,
            service,
            config,
            proxy,
        };
        f.nodes(&[("alpha", "alpha"), ("beta", "beta"), ("gamma", "gamma")])
            .await;
        f.drain().await;
        let status = f.service.status(IndexArgs::default()).await.unwrap();
        assert_eq!(status.done, 3);
        assert_eq!(status.spaces.len(), 1);
        assert_eq!(status.spaces[0].dimension, 2);
        assert_eq!(status.spaces[0].requested_dimensions, None);
        assert_eq!(status.spaces[0].vector_payload_bytes, 24);
        assert!(
            f.proxy
                .requests()
                .iter()
                .all(|request| request.get("dimensions").is_none())
        );
        f
    }
    async fn nodes(&self, nodes: &[(&str, &str)]) {
        let rows = nodes
            .iter()
            .map(|(id, text)| (id.to_string(), text.to_string()))
            .collect::<Vec<_>>();
        let ids = rows.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        self.db.call(move|db|{
            let tx=db.transaction()?;
            for(id,text)in rows {
                let body=json!({"role":"user","content":[{"type":"input_text","text":text}]}).to_string();
                let hash=Sha256::digest(body.as_bytes()).iter().map(|byte|format!("{byte:02x}")).collect::<String>();
                tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,'user','2026-01-01T00:00:00Z',?2,?3,?4)",params![id,body,body.len()as i64,hash])?;
            }
            tx.commit()?;Ok(())
        }).await.unwrap();
        self.service.enqueue_new(&ids).await.unwrap();
    }
    fn rebuild_args(dimensions: Option<usize>) -> IndexArgs {
        IndexArgs {
            dimensions,
            source_ids: vec!["alpha".into(), "beta".into(), "gamma".into()],
            ..Default::default()
        }
    }
    async fn drain(&self) {
        for _ in 0..100 {
            if !self
                .service
                .process_one(CancellationToken::new())
                .await
                .unwrap()
            {
                return;
            }
        }
        panic!("bounded dimension fixture did not drain");
    }
    async fn restart(&mut self) {
        self.db.clone().shutdown().await.unwrap();
        self.db = DbWorker::open(&self.path).await.unwrap();
        self.service = IndexService::new(self.db.clone(), self.config.clone());
    }
    async fn close(self) {
        self.db.shutdown().await.unwrap();
        self.proxy.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_candidate_serializes_first_request_keeps_active_search_and_reuses_its_vector() {
    let mut f = Fixture::new().await;
    let old = f
        .service
        .status(IndexArgs::default())
        .await
        .unwrap()
        .space_id
        .unwrap();
    f.service
        .rebuild(Fixture::rebuild_args(Some(3)))
        .await
        .unwrap();
    let (entered, release) = f.proxy.hold_next_explicit();
    let service = f.service.clone();
    let first = tokio::spawn(async move { service.process_one(CancellationToken::new()).await });
    let request = timeout(Duration::from_secs(60), entered)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request["dimensions"], 3);
    assert_eq!(request["input"].as_array().unwrap().len(), 1);
    assert!(
        !timeout(
            Duration::from_secs(60),
            f.service.process_one(CancellationToken::new())
        )
        .await
        .unwrap()
        .unwrap(),
        "a second worker cannot spend before first validation"
    );
    assert_eq!(
        f.proxy
            .requests()
            .iter()
            .filter(|r| r["dimensions"] == 3)
            .count(),
        1
    );
    assert_eq!(
        f.service
            .status(IndexArgs::default())
            .await
            .unwrap()
            .space_id
            .as_deref(),
        Some(old.as_str())
    );
    assert!(
        !f.service.search("alpha", 10).await.unwrap().is_empty(),
        "old active vectors remain searchable while candidate HTTP is held"
    );
    assert!(
        f.proxy
            .requests()
            .last()
            .unwrap()
            .get("dimensions")
            .is_none()
    );
    release.send(()).unwrap();
    assert!(
        timeout(Duration::from_secs(60), first)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    );
    let interim = f.service.status(IndexArgs::default()).await.unwrap();
    assert_eq!(interim.space_id.as_deref(), Some(old.as_str()));
    let candidate = interim
        .spaces
        .iter()
        .find(|s| s.state == "candidate")
        .unwrap();
    assert_eq!(
        (
            candidate.dimension,
            candidate.requested_dimensions,
            candidate.segments,
            candidate.vector_payload_bytes
        ),
        (3, Some(3), 1, 12)
    );
    f.drain().await;
    let current = f.service.status(IndexArgs::default()).await.unwrap();
    assert_ne!(current.space_id.as_deref(), Some(old.as_str()));
    let active = current.spaces.iter().find(|s| s.selected).unwrap();
    assert_eq!(
        (
            active.connection.as_str(),
            active.model.as_str(),
            active.dimension,
            active.requested_dimensions,
            active.segments,
            active.vector_payload_bytes
        ),
        ("fixture", "synthetic-dimensions", 3, Some(3), 3, 36)
    );
    assert_eq!(
        current
            .spaces
            .iter()
            .find(|s| s.space_id == old)
            .unwrap()
            .state,
        "inactive"
    );
    let explicit = f
        .proxy
        .requests()
        .into_iter()
        .filter(|r| r["dimensions"] == 3)
        .collect::<Vec<_>>();
    assert_eq!(
        explicit
            .iter()
            .map(|r| r["input"].as_array().unwrap().len())
            .sum::<usize>(),
        3,
        "the first real vector is not billed a second time"
    );
    f.restart().await;
    f.nodes(&[("later", "delta")]).await;
    f.drain().await;
    assert_eq!(f.proxy.requests().last().unwrap()["dimensions"], 3);
    assert!(!f.service.search("delta", 10).await.unwrap().is_empty());
    assert_eq!(
        f.proxy.requests().last().unwrap()["dimensions"],
        3,
        "query reuses persisted request mode after restart"
    );
    let before = f.proxy.requests().len();
    f.service
        .rebuild(Fixture::rebuild_args(None))
        .await
        .unwrap();
    f.drain().await;
    assert!(
        f.proxy.requests()[before..]
            .iter()
            .all(|r| r["dimensions"] == 3),
        "omitting N retains a matching active request mode"
    );
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_dimension_refusal_or_mismatch_stops_candidate_without_touching_active_space() {
    for wrong_length in [false, true] {
        let f = Fixture::new().await;
        let old = f
            .service
            .status(IndexArgs::default())
            .await
            .unwrap()
            .space_id
            .unwrap();
        {
            let mut rules = f.proxy.rules.lock().unwrap();
            if wrong_length {
                rules.wrong_length.insert(3);
            } else {
                rules.reject.insert(3);
            }
        }
        f.service
            .rebuild(Fixture::rebuild_args(Some(3)))
            .await
            .unwrap();
        let (entered, release) = f.proxy.hold_next_explicit();
        let service = f.service.clone();
        let first =
            tokio::spawn(async move { service.process_one(CancellationToken::new()).await });
        let request = timeout(Duration::from_secs(60), entered)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request["input"].as_array().unwrap().len(), 1);
        assert!(
            !f.service
                .process_one(CancellationToken::new())
                .await
                .unwrap()
        );
        release.send(()).unwrap();
        assert!(
            timeout(Duration::from_secs(60), first)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        );
        f.drain().await;
        let status = f.service.status(IndexArgs::default()).await.unwrap();
        assert_eq!(status.space_id.as_deref(), Some(old.as_str()));
        assert_eq!(
            status.spaces.len(),
            1,
            "rejected discovery must not create or switch a space"
        );
        assert!(
            status
                .reason
                .as_deref()
                .unwrap()
                .contains("Candidate rebuild stopped")
        );
        assert_eq!(
            f.proxy
                .requests()
                .iter()
                .filter(|r| r["dimensions"] == 3)
                .count(),
            1,
            "no batch/single fallback or native-default retry"
        );
        f.db.call(move|db|{
            let failure=if wrong_length{"dimension_mismatch"}else{"provider"};
            let rows=db.prepare("SELECT value FROM app_meta WHERE key LIKE 'index.dimension_candidate.%'")?.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(rows.len(),1);
            let mode:Value=serde_json::from_str(&rows[0]).unwrap();
            assert_eq!(mode["dimensions"],3);assert_eq!(mode["error"]["kind"],failure);assert!(mode["paused_at"].is_string());
            assert_eq!(db.query_row("SELECT count(*) FROM app_meta WHERE key LIKE 'index.bootstrap.%' AND json_extract(value,'$.state')='failed'",[],|r|r.get::<_,i64>(0))?,3);
            Ok(())
        }).await.unwrap();
        assert!(!f.service.search("alpha", 10).await.unwrap().is_empty());
        assert!(
            f.proxy
                .requests()
                .last()
                .unwrap()
                .get("dimensions")
                .is_none()
        );
        {
            let mut rules = f.proxy.rules.lock().unwrap();
            rules.reject.clear();
            rules.wrong_length.clear();
        }
        f.service.retry(IndexArgs::default()).await.unwrap();
        f.drain().await;
        let status = f.service.status(IndexArgs::default()).await.unwrap();
        assert_ne!(status.space_id.as_deref(), Some(old.as_str()));
        assert_eq!(
            status
                .spaces
                .iter()
                .find(|s| s.selected)
                .unwrap()
                .requested_dimensions,
            Some(3)
        );
        f.close().await;
    }
}

#[tokio::test]
async fn probe_records_one_request_per_call_replaces_its_tuple_and_keeps_only_newest_observations()
{
    let f = Fixture::new().await;
    let initial = f.service.status(IndexArgs::default()).await.unwrap();
    let before = f.proxy.requests().len();
    for _ in 0..2 {
        let result = f
            .service
            .probe(IndexProbeArgs {
                dimensions: Some(3),
            })
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.dimension, Some(3));
        assert_eq!(result.requested_dimensions, Some(3));
        let status = f.service.status(IndexArgs::default()).await.unwrap();
        assert_eq!(status.probes.len(), 1);
    }
    assert_eq!(f.proxy.requests().len(), before + 2);
    f.proxy.rules.lock().unwrap().reject.insert(7);
    for n in 1..=34 {
        if n == 3 {
            continue;
        }
        let before = f.proxy.requests().len();
        let result = f
            .service
            .probe(IndexProbeArgs {
                dimensions: Some(n),
            })
            .await;
        assert_eq!(result.is_err(), n == 7);
        assert_eq!(
            f.proxy.requests().len(),
            before + 1,
            "one fixed input, no hidden retry"
        );
        let request = f.proxy.requests().pop().unwrap();
        assert_eq!(
            request["input"],
            json!(["Shellboy embedding connection probe."])
        );
        assert_eq!(request["dimensions"], n);
        if n == 7 {
            let status = f.service.status(IndexArgs::default()).await.unwrap();
            let failed = status
                .probes
                .iter()
                .find(|p| p.requested_dimensions == Some(7))
                .unwrap();
            assert!(!failed.success);
            assert_eq!(failed.error.as_ref().unwrap()["status"], 400);
        }
    }
    let status = f.service.status(IndexArgs::default()).await.unwrap();
    assert_eq!(status.probes.len(), 32);
    assert_eq!(status.space_id, initial.space_id);
    assert_eq!(status.done, initial.done);
    assert!(!status.paused, "a failed probe does not pause active work");
    f.db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM app_meta WHERE key LIKE 'index.probe.%'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            32
        );
        Ok(())
    })
    .await
    .unwrap();
    f.close().await;
}

#[tokio::test]
async fn probe_mutation_receipt_replays_without_another_embedding_request() {
    let proxy = Proxy::new().await;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,format!("[connections.fixture]\nbase_url={:?}\n[embedding]\nconnection='fixture'\nmodel='synthetic-dimensions'\n",proxy.url)).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let operation = format!("{}:{}", owner.instance_id, uuid::Uuid::new_v4());
    let command = Command::IndexProbe(IndexProbeArgs {
        dimensions: Some(3),
    });
    let first = control
        .request(Request::new(operation.clone(), command.clone()))
        .await;
    assert!(first.ok, "{first:?}");
    let replay = control
        .request(Request::new(operation.clone(), command))
        .await;
    assert_eq!(replay.result, first.result);
    assert!(replay.ok, "{replay:?}");
    assert_eq!(proxy.requests().len(), 1);
    let conflict = control
        .request(Request::new(
            operation,
            Command::IndexProbe(IndexProbeArgs {
                dimensions: Some(4),
            }),
        ))
        .await;
    assert_eq!(conflict.error.unwrap().code, ErrorCode::OperationConflict);
    assert_eq!(proxy.requests().len(), 1);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    proxy.close().await;
}

#[tokio::test]
async fn first_transient_dimension_failure_preserves_retry_and_requested_mode() {
    let f = Fixture::new().await;
    let old = f
        .service
        .status(IndexArgs::default())
        .await
        .unwrap()
        .space_id
        .unwrap();
    f.proxy.rules.lock().unwrap().transient_once.insert(3);
    let before = f.proxy.requests().len();
    f.service
        .rebuild(Fixture::rebuild_args(Some(3)))
        .await
        .unwrap();
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    let requests = f.proxy.requests();
    assert_eq!(requests.len(), before + 1);
    assert_eq!(requests.last().unwrap()["dimensions"], 3);
    assert_eq!(
        requests.last().unwrap()["input"].as_array().unwrap().len(),
        1
    );
    let status = f.service.status(IndexArgs::default()).await.unwrap();
    assert_eq!(status.space_id.as_deref(), Some(old.as_str()));
    assert!(!status.paused);
    f.db.call(|db| {
        let mode: String = db.query_row("SELECT value FROM app_meta WHERE key LIKE 'index.dimension_candidate.%'", [], |r| r.get(0))?;
        let mode: Value = serde_json::from_str(&mode).unwrap();
        assert!(mode["error"].is_null());
        assert!(mode["paused_at"].is_null());
        let work: String = db.query_row("SELECT value FROM app_meta WHERE key LIKE 'index.bootstrap.%' AND json_extract(value,'$.attempts')=1", [], |r|r.get(0))?;
        let work: Value = serde_json::from_str(&work).unwrap();
        assert_eq!(work["state"], "pending");
        assert_eq!(work["error"]["status"], 500);
        // Move only the synthetic retry deadline; no wall-clock sleep or production clock hook.
        db.execute("UPDATE app_meta SET value=json_set(value,'$.next_at',NULL) WHERE key LIKE 'index.bootstrap.%' AND json_extract(value,'$.attempts')=1", [])?;
        Ok(())
    }).await.unwrap();
    f.drain().await;
    let status = f.service.status(IndexArgs::default()).await.unwrap();
    assert_ne!(status.space_id.as_deref(), Some(old.as_str()));
    let active = status.spaces.iter().find(|s| s.selected).unwrap();
    assert_eq!(
        (
            active.dimension,
            active.requested_dimensions,
            active.segments
        ),
        (3, Some(3), 3)
    );
    assert!(
        f.proxy.requests()[before..]
            .iter()
            .all(|r| r["dimensions"] == 3)
    );
    f.close().await;
}

#[tokio::test]
async fn omitted_dimensions_for_changed_model_start_in_native_mode() {
    let mut f = Fixture::new().await;
    f.service
        .rebuild(Fixture::rebuild_args(Some(3)))
        .await
        .unwrap();
    f.drain().await;
    let mut config = (*f.config).clone();
    config.embedding.model = Some("synthetic-other-model".into());
    f.config = Arc::new(config);
    f.restart().await;
    let before = f.proxy.requests().len();
    f.service
        .rebuild(Fixture::rebuild_args(None))
        .await
        .unwrap();
    f.drain().await;
    let requests = f.proxy.requests();
    assert!(requests.len() > before);
    assert!(
        requests[before..]
            .iter()
            .all(|r| r.get("dimensions").is_none())
    );
    assert!(
        requests[before..]
            .iter()
            .all(|r| r["model"] == "synthetic-other-model")
    );
    let status = f.service.status(IndexArgs::default()).await.unwrap();
    let active = status.spaces.iter().find(|s| s.selected).unwrap();
    assert_eq!(
        (
            active.model.as_str(),
            active.dimension,
            active.requested_dimensions,
            active.segments
        ),
        ("synthetic-other-model", 2, None, 3)
    );
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn superseded_dimension_failure_cannot_stop_or_pause_new_candidate() {
    let f = Fixture::new().await;
    let old = f
        .service
        .status(IndexArgs::default())
        .await
        .unwrap()
        .space_id
        .unwrap();
    f.proxy.rules.lock().unwrap().reject.insert(3);
    f.service
        .rebuild(Fixture::rebuild_args(Some(3)))
        .await
        .unwrap();
    let before = f.proxy.requests().len();
    let (entered, release) = f.proxy.hold_next_explicit();
    let service = f.service.clone();
    let first = tokio::spawn(async move { service.process_one(CancellationToken::new()).await });
    let request = timeout(Duration::from_secs(60), entered)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request["dimensions"], 3);
    assert_eq!(request["input"].as_array().unwrap().len(), 1);
    f.service
        .rebuild(Fixture::rebuild_args(Some(4)))
        .await
        .unwrap();
    release.send(()).unwrap();
    assert!(
        timeout(Duration::from_secs(60), first)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    );
    let status = f.service.status(IndexArgs::default()).await.unwrap();
    assert_eq!(status.space_id.as_deref(), Some(old.as_str()));
    assert!(
        !status.paused,
        "superseded failure cannot pause the endpoint"
    );
    f.db.call(|db| {
        let mode: String = db.query_row(
            "SELECT value FROM app_meta WHERE key LIKE 'index.dimension_candidate.%'",
            [],
            |r| r.get(0),
        )?;
        let mode: Value = serde_json::from_str(&mode).unwrap();
        assert_eq!(mode["dimensions"], 4);
        assert!(mode["error"].is_null());
        assert!(mode["paused_at"].is_null());
        Ok(())
    })
    .await
    .unwrap();
    f.drain().await;
    let status = f.service.status(IndexArgs::default()).await.unwrap();
    let active = status.spaces.iter().find(|s| s.selected).unwrap();
    assert_eq!(
        (
            active.dimension,
            active.requested_dimensions,
            active.segments
        ),
        (4, Some(4), 3)
    );
    assert_ne!(status.space_id.as_deref(), Some(old.as_str()));
    assert!(!status.paused);
    let requests = f.proxy.requests();
    assert_eq!(requests[before]["dimensions"], 3);
    assert!(requests.len() > before + 1);
    assert!(requests[before + 1..].iter().all(|r| r["dimensions"] == 4));
    assert_eq!(
        requests[before + 1..]
            .iter()
            .map(|r| r["input"].as_array().unwrap().len())
            .sum::<usize>(),
        3
    );
    f.close().await;
}
