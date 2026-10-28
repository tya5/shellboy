//! Opt-in real 20k-chunk ranking/writer contention measurement; no provider access.
use super::*;
use std::{sync::Mutex, time::Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static STARTED: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);
pub(super) fn ranking_started() {
    if let Some(send) = STARTED.lock().unwrap().take() {
        let _ = send.send(());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "20,000 x 1536 synthetic vector contention measurement; run serially"]
async fn performance_semantic_ranking_writer_contention() {
    let temp = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = Config::default();
    config.connections.insert(
        "fixture".into(),
        crate::config::Connection {
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            ..Default::default()
        },
    );
    config.embedding.connection = Some("fixture".into());
    config.embedding.model = Some("synthetic".into());
    let config = Arc::new(config);
    let config_path = temp.path().join("config.toml");
    std::fs::write(&config_path, format!("default_connection = \"fixture\"\ndefault_model = \"synthetic\"\n[connections.fixture]\nbase_url = {:?}\n[embedding]\nconnection = \"fixture\"\nmodel = \"synthetic\"\n", config.connections["fixture"].base_url)).unwrap();
    let owner = crate::app::DataOwner::start(
        Some(config_path),
        Some(temp.path().join("data")),
        temp.path().into(),
    )
    .await
    .unwrap();
    let db = owner.db.clone();
    let control = crate::control::Control::new(&owner, temp.path().into());
    let service = IndexService::new(db.clone(), config.clone());
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 8192];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let len = header
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let body=json!({"data":[{"index":0,"embedding":vec![0.1f32;1536]}],"usage":{"prompt_tokens":1,"total_tokens":1}}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    db.call(move |db| {
        let tx=db.transaction()?;
        let identity=identity::EmbeddingIdentity::configured(&config,identity::profile_key(&config).unwrap());
        let space=ensure_space(&tx,&identity,"active",1536)?;
        tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('probe','/','/a','fixture','synthetic','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('probe-run','probe','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('probe-turn','probe','probe-run','running','now'); INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('probe-shell','probe-run','probe-turn','probe-operation','running','/','{}','UTF-8','UTF-8','open','now');")?;
        let first=serde_json::to_string(&vec![0.1f32;1536]).unwrap();
        let mut node=tx.prepare("INSERT INTO node(id,kind,created_at,payload_json,body_hash)VALUES(?1,'user','2026-10-28T00:00:00Z','{\"text\":\"synthetic\"}','fixture-body')")?;
        let mut job=tx.prepare("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,authorized_watermark)VALUES(?1,?2,?3,0,'canonical','fixture-body',0,9,'done',?4)")?;
        let mut chunk=tx.prepare("INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id)VALUES(?1,?1,?2,?3,0,0,9,'fixture-body',?4)")?;
        let mut vector=tx.prepare(&format!("INSERT INTO {}(rowid,embedding)VALUES(?1,?2)",space.table))?;
        for i in 0..20_000i64 {
            let id=format!("n{i:05}"); let item=format!("c{i:05}");
            node.execute([&id])?;
            let watermark=json!({"witness":{"node_id":id,"revision":0,"body_hash":"fixture-body","raw":null}}).to_string();
            job.execute(params![item,space.id,id,watermark])?;
            vector.execute(params![i+1,first])?;
            chunk.execute(params![item,space.id,id,i+1])?;
        }
        drop((node,job,chunk,vector)); tx.commit()?; Ok(())
    }).await.unwrap();
    let mut samples = Vec::new();
    for sequence in 0..5i64 {
        let (send, receive) = tokio::sync::oneshot::channel();
        *STARTED.lock().unwrap() = Some(send);
        let searching = service.clone();
        let search = tokio::spawn(async move {
            let started = Instant::now();
            let hits = searching.search_detailed("synthetic", 10).await.unwrap();
            assert_eq!(hits.len(), 10);
            started.elapsed()
        });
        tokio::time::timeout(Duration::from_secs(30), receive)
            .await
            .unwrap()
            .unwrap();
        let writer = db.clone();
        let output = tokio::spawn(async move {
            let started = Instant::now();
            writer.call(move |db| {
                let tx=db.transaction()?;
                tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('probe-shell',?1,'stdout',?1,x'78','now')",[sequence])?;
                tx.execute("UPDATE shell_execution SET saved_sequence=?1 WHERE id='probe-shell'",[sequence+1])?;
                tx.commit()?; Ok(())
            }).await.unwrap();
            started.elapsed()
        });
        let reading = control.clone();
        let get = tokio::spawn(async move {
            let started = Instant::now();
            let request = crate::protocol::Request::new(
                format!("{}:{}", reading.store.instance_id, uuid::Uuid::new_v4()),
                crate::protocol::Command::ThreadGet(crate::protocol::ThreadTarget {
                    thread_id: Some("probe".into()),
                }),
            );
            let response = reading.request(request).await;
            assert!(response.ok, "{:?}", response.error);
            started.elapsed()
        });
        let (search, output, get) = tokio::join!(search, output, get);
        samples.push(json!({"ranking_ms":search.unwrap().as_secs_f64()*1000.0,"shell_output_commit_ms":output.unwrap().as_secs_f64()*1000.0,"thread_read_ms":get.unwrap().as_secs_f64()*1000.0}));
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    *STARTED.lock().unwrap() = Some(send);
    let cancelled = CancellationToken::new();
    let observer = service.clone();
    let caller = cancelled.clone();
    let searching = tokio::spawn(async move {
        observer
            .search_filtered_cancellable(
                &HistorySearchArgs {
                    query: "cancel synthetic".into(),
                    limit: 10,
                    ..Default::default()
                },
                caller,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(30), receive)
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let busy = service.search("busy must not embed", 10).await.unwrap_err();
    assert_eq!(busy.code, ErrorCode::InvalidState);
    assert!(busy.message.contains("already running"));
    let cancelling = Instant::now();
    cancelled.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), searching)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    tokio::time::timeout(Duration::from_secs(2), service.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let cancellation_ms = cancelling.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(service.search_slots.available_permits(), 1);
    let (send, receive) = tokio::sync::oneshot::channel();
    *STARTED.lock().unwrap() = Some(send);
    let observer = service.clone();
    let dropped = tokio::spawn(async move { observer.search("drop synthetic", 10).await });
    tokio::time::timeout(Duration::from_secs(30), receive)
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let dropping = Instant::now();
    dropped.abort();
    assert!(dropped.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), service.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let drop_drain_ms = dropping.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(service.search_slots.available_permits(), 1);
    println!(
        "PERF {}",
        json!({"name":"semantic_20k_writer_contention","chunks":20000,"dimensions":1536,"samples":samples,"cancellation_and_drain_ms":cancellation_ms,"drop_and_drain_ms":drop_drain_ms,"scope":"Real IndexService.search_detailed and sqlite-vec ranking; concurrent output_chunk/saved_sequence commit and full Control ThreadGet projection; loopback embedding mock"})
    );
    service.cancel();
    service.wait_idle().await.unwrap();
    server.abort();
    let _ = server.await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
