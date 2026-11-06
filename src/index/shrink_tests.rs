//! Int8 conversion on temporary databases. Providers are local fixtures that
//! count requests; nothing here reaches a real API.
use super::*;
use crate::storage::{BACKGROUND_STEP_BUDGET, DbWorker};
use std::{collections::BTreeMap, sync::atomic::AtomicUsize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    temp: tempfile::TempDir,
    db: DbWorker,
    config: Arc<Config>,
    service: Arc<IndexService>,
    source: Space,
    requests: Arc<AtomicUsize>,
}

/// Answers every embedding request with one fixed vector and counts them.
async fn embedding_server(vector: Vec<f32>) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (counted, vector) = (counted.clone(), vector.clone());
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                        continue;
                    };
                    let header = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let length = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .map_or(0, |value| value.trim().parse::<usize>().unwrap());
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
                counted.fetch_add(1, Ordering::SeqCst);
                let body = json!({"data":[{"index":0,"embedding":vector}],"usage":{"prompt_tokens":1,"total_tokens":1}}).to_string();
                let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            });
        }
    });
    (url, requests)
}

fn unit_vector(values: impl IntoIterator<Item = f32>) -> Vec<f32> {
    crate::model::http::unit_normalized(values.into_iter().collect()).unwrap()
}

/// Deterministic clustered unit vectors, so nearest neighbours are meaningful.
fn clustered(count: usize, dimension: usize) -> Vec<Vec<f32>> {
    let mut state = 253u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 33) as f32 / (1u64 << 31) as f32) - 0.5
    };
    let centers: Vec<Vec<f32>> = (0..16)
        .map(|_| (0..dimension).map(|_| next()).collect())
        .collect();
    (0..count)
        .map(|i| {
            let center = &centers[i % centers.len()];
            unit_vector(center.iter().map(|value| value + next() * 0.5))
        })
        .collect()
}

fn seed_rows(tx: &Transaction<'_>, space: &Space, start: usize, vectors: &[Vec<f32>]) {
    for (offset, vector) in vectors.iter().enumerate() {
        let i = start + offset;
        let (node, watermark) = (
            format!("node-{i:05}"),
            json!({"witness":{"node_id":format!("node-{i:05}"),"revision":0,"body_hash":"body","raw":null}}).to_string(),
        );
        tx.execute("INSERT INTO node(id,kind,created_at,payload_json,body_hash)VALUES(?1,'user','2026-11-07T00:00:00Z','{}','body')", [&node]).unwrap();
        tx.execute(
            "INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('thread',?1,?2)",
            params![i as i64, node],
        )
        .unwrap();
        let job = format!("job-{}-{i}", space.id);
        tx.execute("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,authorized_watermark)VALUES(?1,?2,?3,0,'canonical','body',0,4,'done',?4)", params![job, space.id, node, watermark]).unwrap();
        let row = i as i64 + 1;
        tx.execute(
            &format!("INSERT INTO {}(rowid,embedding)VALUES(?1,?2)", space.table),
            params![row, serde_json::to_string(vector).unwrap()],
        )
        .unwrap();
        tx.execute("INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id)VALUES(?1,?1,?2,?3,0,0,4,'body',?4)", params![job, space.id, node, row]).unwrap();
        tx.execute(
            "INSERT INTO app_meta(key,value)VALUES(?1,?2)",
            params![IndexKeys::preview(&job), format!("preview {i}")],
        )
        .unwrap();
    }
}

async fn fixture(vectors: Vec<Vec<f32>>, answer: Vec<f32>) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let db = DbWorker::open(temp.path().join("shrink.sqlite"))
        .await
        .unwrap();
    let (url, requests) = embedding_server(answer).await;
    let mut config = Config::default();
    config.connections.insert(
        "fixture".into(),
        crate::config::Connection {
            base_url: url,
            ..Default::default()
        },
    );
    config.embedding.connection = Some("fixture".into());
    config.embedding.model = Some("synthetic".into());
    let config = Arc::new(config);
    let (seeded, dimension) = (config.clone(), vectors.first().map_or(2, Vec::len));
    let source = db
        .call(move |db| {
            let tx = db.transaction()?;
            let identity = identity::EmbeddingIdentity::configured(
                &seeded,
                identity::profile_key(&seeded).unwrap(),
            );
            let space = store::ensure_space(&tx, &identity, "active", dimension)?;
            tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread','/','/a','fixture','synthetic','now');")?;
            seed_rows(&tx, &space, 0, &vectors);
            tx.commit()?;
            Ok(space)
        })
        .await
        .unwrap();
    let service = IndexService::new(db.clone(), config.clone());
    Fixture {
        temp,
        db,
        config,
        service,
        source,
        requests,
    }
}

impl Fixture {
    async fn start(&self) {
        self.service.shrink(Default::default()).await.unwrap();
    }
    /// Run until no unit can progress; returns the units that did something.
    async fn drain(&self) -> usize {
        let mut units = 0;
        while self.service.shrink_unit().await.unwrap() {
            units += 1;
        }
        units
    }
    async fn active(&self) -> Space {
        self.db
            .call(|db| {
                let id = meta(db, IndexKeys::ACTIVE_SPACE)?.unwrap();
                Ok(load_space(db, &id)?.unwrap())
            })
            .await
            .unwrap()
    }
    async fn record(&self) -> Option<Record> {
        self.db.call(|db| read_record(&*db)).await.unwrap()
    }
    async fn space(&self, id: &str) -> Option<Space> {
        let id = id.to_owned();
        self.db.call(move |db| load_space(db, &id)).await.unwrap()
    }
    async fn count(&self, sql: &'static str, space: &str) -> i64 {
        let space = space.to_owned();
        self.db
            .call(move |db| Ok(db.query_row(sql, [space], |row| row.get(0))?))
            .await
            .unwrap()
    }
    /// Rank through the production query on one space, chosen explicitly.
    async fn rank(&self, space: &Space, query: &[f32], limit: u32) -> Vec<IndexHit> {
        let (space, encoded) = (space.clone(), space.encode_vector(query).unwrap());
        self.db
            .call(move |db| {
                Ok(search::filtered_hits(
                    db,
                    &space,
                    &encoded,
                    &HistorySearchArgs {
                        query: "fixture".into(),
                        limit,
                        ..Default::default()
                    },
                )
                .unwrap())
            })
            .await
            .unwrap()
    }
}

const CHUNKS: &str = "SELECT count(*) FROM embedding_chunk WHERE space_id=?1";

#[test]
fn quantize_scales_rounds_and_saturates_symmetrically() {
    assert_eq!(
        quantize(&[0.0, 0.1, -0.1, 0.2, -0.2, 0.5, -0.5, 1e30], 635.0),
        vec![0, 64, -64, 127, -127, 127, -127, 127]
    );
}

/// The pinned sqlite-vec must do everything the int8 path relies on.
#[test]
fn pinned_sqlite_vec_supports_int8_knn_rowid_filter_exact_and_delete() {
    crate::storage::register_vec().unwrap();
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE VIRTUAL TABLE v USING vec0(embedding int8[2]);INSERT INTO v(rowid,embedding)VALUES(1,vec_int8('[0,0]')),(2,vec_int8('[3,4]')),(3,vec_int8('[100,0]')),(4,vec_int8('[6,8]'));").unwrap();
    let knn = |db: &Connection, sql: &str| -> Vec<(i64, f64)> {
        db.prepare(sql)
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        knn(
            &db,
            "SELECT rowid,distance FROM v WHERE embedding MATCH vec_int8('[0,0]') AND k=3 ORDER BY distance"
        ),
        vec![(1, 0.0), (2, 5.0), (4, 10.0)]
    );
    assert_eq!(
        knn(
            &db,
            "SELECT rowid,distance FROM v WHERE embedding MATCH vec_int8('[0,0]') AND k=2 AND rowid IN (SELECT value FROM json_each('[3,4]')) ORDER BY distance"
        ),
        vec![(4, 10.0), (3, 100.0)]
    );
    assert_eq!(
        knn(
            &db,
            "SELECT rowid,vec_distance_L2(embedding,vec_int8('[0,0]')) d FROM v WHERE rowid IN (SELECT value FROM json_each('[2,4]')) ORDER BY d"
        ),
        vec![(2, 5.0), (4, 10.0)]
    );
    // Without the subtype a JSON query is float32 and is refused, never mixed.
    assert!(
        db.prepare("SELECT rowid FROM v WHERE embedding MATCH '[0,0]' AND k=1")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .next()
            .unwrap()
            .is_err()
    );
    db.execute("DELETE FROM v WHERE rowid=2", []).unwrap();
    assert_eq!(
        knn(
            &db,
            "SELECT rowid,distance FROM v WHERE embedding MATCH vec_int8('[0,0]') AND k=2 ORDER BY distance"
        ),
        vec![(1, 0.0), (4, 10.0)]
    );
    let stored: Vec<u8> = db
        .query_row("SELECT embedding FROM v WHERE rowid=3", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        stored,
        vec![100, 0],
        "one byte per element, read back as a blob"
    );
}

#[tokio::test]
async fn conversion_calls_no_api_keeps_old_space_serving_and_preserves_rank() {
    let vectors = clustered(600, 128);
    let f = fixture(vectors.clone(), vec![1.0; 128]).await;
    let preview = f
        .service
        .shrink(IndexShrinkArgs {
            dry_run: true,
            cancel: false,
        })
        .await
        .unwrap();
    assert_eq!(preview["state"], "ready");
    assert_eq!(preview["segments"], 600);
    assert_eq!(preview["vector_payload_bytes"], 600 * 128 * 4);
    assert_eq!(preview["vector_payload_bytes_after"], 600 * 128);
    assert_eq!(preview["embedding_requests"], 0);
    assert!(f.record().await.is_none(), "a preview changes nothing");
    f.start().await;
    // Until complete the old float space alone serves search.
    while f.active().await.id == f.source.id {
        let hits = f.rank(&f.source, &vectors[7], 1).await;
        assert_eq!(hits[0].node_id, "node-00007");
        assert_eq!(f.active().await.state, "active");
        assert!(f.service.shrink_unit().await.unwrap());
    }
    f.drain().await;
    assert!(
        f.record().await.is_none(),
        "the record ends with the conversion"
    );
    let active = f.active().await;
    let max = vectors
        .iter()
        .flatten()
        .fold(0f64, |max, v| max.max(f64::from(v.abs())));
    assert_eq!(
        active.int8_scale,
        Some(127.0 / max),
        "scale comes from the data"
    );
    assert_eq!(active.state, "active");
    assert_eq!(active.dimension, 128);
    assert_eq!(f.space(&f.source.id).await.unwrap().state, "inactive");
    assert_eq!(f.count(CHUNKS, &active.id).await, 600);
    assert_eq!(
        f.requests.load(Ordering::SeqCst),
        0,
        "no embedding API call"
    );
    // Rank agreement with duplicates absent by construction; self excluded.
    let (mut same, mut total) = (0, 0);
    for query in vectors.iter().step_by(13) {
        let ids = |hits: Vec<IndexHit>| {
            hits.into_iter()
                .skip(1)
                .map(|h| h.node_id)
                .collect::<HashSet<_>>()
        };
        let float = ids(f.rank(&f.source, query, 11).await);
        let int8 = ids(f.rank(&active, query, 11).await);
        same += float.intersection(&int8).count();
        total += float.len();
    }
    let agreement = same as f64 / total as f64;
    println!("synthetic rank agreement top-10: {agreement:.4}");
    assert!(agreement >= 0.9, "{agreement}");
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn query_uses_saved_scale_and_distance_is_l2_of_the_stored_integers() {
    let vectors = vec![
        unit_vector([1.0, 0.0]),
        unit_vector([0.6, 0.8]),
        unit_vector([0.0, 1.0]),
    ];
    let f = fixture(vectors, unit_vector([0.6, 0.8])).await;
    f.start().await;
    f.drain().await;
    let active = f.active().await;
    assert_eq!(active.int8_scale, Some(127.0));
    let before = f.requests.load(Ordering::SeqCst);
    let hits = f.service.search_detailed("query text", 3).await.unwrap();
    assert_eq!(f.requests.load(Ordering::SeqCst), before + 1);
    assert_eq!(hits[0].node_id, "node-00001");
    assert_eq!(hits[0].distance, 0.0);
    // [76,102] to [127,0] and to [0,127]: integer L2, not a unit-length distance.
    let expect = |a: f64, b: f64| ((a * a + b * b).sqrt() as f32) as f64;
    assert_eq!(hits[1].node_id, "node-00002");
    assert_eq!(hits[1].distance, expect(76.0, 25.0));
    assert_eq!(hits[2].distance, expect(51.0, 102.0));
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn interruption_resumes_at_the_saved_cursor_after_reopening() {
    let vectors = clustered(100, 16);
    let f = fixture(vectors.clone(), vec![1.0; 16]).await;
    let path = f.db.path().to_path_buf();
    f.start().await;
    // Scale pass (4 pages + empty) and part of the copy.
    for _ in 0..7 {
        assert!(f.service.shrink_unit().await.unwrap());
    }
    let saved = f.record().await.unwrap();
    assert_eq!(saved.phase, COPY);
    assert_eq!(saved.cursor, 2 * UNIT.get() as i64);
    let target = saved.target.clone().unwrap();
    assert_eq!(f.count(CHUNKS, &target).await, saved.cursor);
    let Fixture {
        temp,
        db,
        config,
        source,
        ..
    } = f;
    db.shutdown().await.unwrap();
    let db = DbWorker::open(path).await.unwrap();
    let service = IndexService::new(db.clone(), config.clone());
    let active = db
        .call(|db| meta(db, IndexKeys::ACTIVE_SPACE))
        .await
        .unwrap();
    assert_eq!(
        active.as_deref(),
        Some(source.id.as_str()),
        "still the old space"
    );
    while service.shrink_unit().await.unwrap() {}
    let target_id = target.clone();
    let (rows, stored): (i64, Vec<(i64, Vec<u8>)>) = db
        .call(move |db| {
            let space = load_space(db, &target_id)?.unwrap();
            let rows = db.query_row(CHUNKS, [&target_id], |r| r.get(0))?;
            let mut q = db.prepare(&format!(
                "SELECT rowid,embedding FROM {} ORDER BY rowid",
                space.table
            ))?;
            let stored = q
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok((rows, stored))
        })
        .await
        .unwrap();
    assert_eq!(rows, 100);
    assert_eq!(stored.len(), 100);
    let max = vectors
        .iter()
        .flatten()
        .fold(0f64, |max, v| max.max(f64::from(v.abs())));
    for (row, bytes) in stored {
        let expected = quantize(&vectors[row as usize - 1], 127.0 / max);
        assert_eq!(
            bytes,
            expected.iter().map(|v| *v as u8).collect::<Vec<_>>(),
            "row {row}"
        );
    }
    db.shutdown().await.unwrap();
    drop(temp);
}

#[tokio::test]
async fn failure_keeps_the_old_space_and_an_explicit_run_resumes() {
    let vectors = clustered(100, 16);
    let f = fixture(vectors.clone(), vec![1.0; 16]).await;
    let source = f.source.id.clone();
    let trigger = source.clone();
    f.db.call(move |db| {
        db.execute_batch(&format!("CREATE TRIGGER refuse_copy BEFORE INSERT ON embedding_chunk WHEN NEW.vec_row_id=50 AND NEW.space_id<>'{trigger}' BEGIN SELECT RAISE(ABORT,'synthetic copy failure'); END;"))?;
        Ok(())
    }).await.unwrap();
    f.start().await;
    f.drain().await;
    let failed = f.record().await.unwrap();
    assert!(
        failed.error.as_ref().unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("synthetic copy failure"),
        "{failed:?}"
    );
    assert_eq!(failed.phase, COPY);
    assert_eq!(f.active().await.id, source);
    assert_eq!(
        f.rank(&f.source, &vectors[3], 1).await[0].node_id,
        "node-00003"
    );
    assert!(
        !f.service.shrink_unit().await.unwrap(),
        "a failure waits for an explicit run"
    );
    let status = f.service.status(Default::default()).await.unwrap();
    assert!(status.shrink.as_ref().unwrap().error.is_some());
    assert_eq!(
        f.service
            .shrink(IndexShrinkArgs {
                dry_run: true,
                cancel: false
            })
            .await
            .unwrap()["state"],
        "failed"
    );
    f.db.call(|db| {
        db.execute_batch("DROP TRIGGER refuse_copy")?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.service.shrink(Default::default()).await.unwrap()["state"],
        "resumed"
    );
    f.drain().await;
    assert!(f.record().await.is_none());
    let active = f.active().await;
    assert_ne!(active.id, source);
    assert_eq!(f.count(CHUNKS, &active.id).await, 100);
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn out_of_scale_embeddings_saturate_after_conversion() {
    let f = fixture(
        vec![unit_vector([0.6, 0.8]), unit_vector([0.8, 0.6])],
        vec![1.0, 0.0],
    )
    .await;
    f.start().await;
    f.drain().await;
    let active = f.active().await;
    assert_eq!(active.int8_scale, Some(127.0 / f64::from(0.8f32)));
    let (space, work) = (
        active.clone(),
        Work::pending(
            Witness {
                node_id: "node-00000".into(),
                revision: 0,
                body_hash: Some("body".into()),
                raw: None,
            },
            "extra".into(),
            Segment {
                start: 0,
                end: 1,
                text: "x".into(),
            },
            "active",
            "explicit",
        ),
    );
    let table = active.table.clone();
    let stored: Vec<u8> =
        f.db.call(move |db| {
            let tx = db.transaction()?;
            queue_job(&tx, &space, &work)?;
            let job = job_id_for(&tx, &space, &work)?.unwrap();
            store::adopt_vector_tx(&tx, &space, &job, &work, &[1.0, -1.0])?;
            tx.commit()?;
            Ok(db.query_row(
                &format!("SELECT embedding FROM {table} WHERE rowid=3"),
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(stored, vec![127, (-127i8) as u8]);
    // A query past the scale clips the same way and still ranks.
    let hits = f.rank(&active, &[1.0, 0.0], 3).await;
    assert_eq!(hits.len(), 2);
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn exact_fallback_and_thread_filter_rank_int8() {
    // More than 4096 nearest chunks of one node force the exact path.
    let temp = tempfile::tempdir().unwrap();
    let db = DbWorker::open(temp.path().join("exact.sqlite"))
        .await
        .unwrap();
    let hits = db.call(|db| {
        let tx = db.transaction()?;
        tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread','/','/a','p','m','now');INSERT INTO node(id,kind,created_at,payload_json,body_hash)VALUES('near','user','2026-11-07T00:00:00Z','{}','body'),('far','user','2026-11-07T00:00:00Z','{}','body');INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('thread',0,'near'),('thread',1,'far');CREATE VIRTUAL TABLE shellboy_vec_00000000000000000000000000000001 USING vec0(embedding int8[2]);INSERT INTO index_space(id,profile_id,model_id,dimension,splitter_version,state,vec_table_name,int8_scale)VALUES('s','p','m',2,'v','active','shellboy_vec_00000000000000000000000000000001',127.0);")?;
        let space = load_space(&tx, "s")?.unwrap();
        for i in 0..4101i64 {
            let (node, vector) = if i < 4100 { ("near", "[127,1]") } else { ("far", "[0,127]") };
            let watermark = json!({"witness":{"node_id":node,"revision":0,"body_hash":"body","raw":null}}).to_string();
            tx.execute("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,authorized_watermark)VALUES(?1,'s',?2,0,'canonical','body',?3,?3,'done',?4)", params![format!("j{i}"), node, i, watermark])?;
            tx.execute("INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id)VALUES(?1,?1,'s',?2,0,?3,?3,'body',?4)", params![format!("j{i}"), node, i, i + 1])?;
            tx.execute(&format!("INSERT INTO {}(rowid,embedding)VALUES(?1,vec_int8(?2))", space.table), params![i + 1, vector])?;
        }
        tx.commit()?;
        let query = space.encode_vector(&[1.0, 0.0]).map_err(|_| serde_error())?;
        let args = HistorySearchArgs { query: "q".into(), limit: 2, ..Default::default() };
        let hits = search::filtered_hits(db, &space, &query, &args).unwrap();
        // A Thread filter narrows the rowid IN set of the same int8 KNN.
        db.execute("DELETE FROM thread_path WHERE node_id='near'", [])?;
        let filtered = search::filtered_hits(db, &space, &query, &HistorySearchArgs { query: "q".into(), limit: 2, thread_id: Some("thread".into()), ..Default::default() }).unwrap();
        Ok((hits, filtered))
    }).await.unwrap();
    assert_eq!(
        hits.0
            .iter()
            .map(|h| h.node_id.as_str())
            .collect::<Vec<_>>(),
        ["near", "far"]
    );
    assert_eq!(hits.0[0].distance, 1.0);
    assert_eq!(hits.0[0].spans.len(), 4100, "exact path groups every span");
    assert!((hits.0[1].distance - (2.0f64 * 127.0 * 127.0).sqrt()).abs() < 1e-3);
    assert_eq!(
        hits.1
            .iter()
            .map(|h| h.node_id.as_str())
            .collect::<Vec<_>>(),
        ["far"]
    );
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn changes_during_conversion_are_never_lost() {
    let vectors = clustered(40, 8);
    let f = fixture(vectors.clone(), vec![1.0; 8]).await;
    f.start().await;
    // Finish the scale pass and the whole copy, stopping before the switch.
    while f.record().await.unwrap().cursor < 40 || f.record().await.unwrap().phase != COPY {
        assert!(f.service.shrink_unit().await.unwrap());
    }
    let source = f.source.clone();
    let added = unit_vector((0..8).map(|i| if i == 3 { 1.0 } else { 0.0 }));
    let adopted = added.clone();
    // Delete the newest source, then adopt: MAX+1 would reuse the row behind the cursor.
    let row: i64 = f.db.call(move |db| {
        let tx = db.transaction()?;
        store::delete_source_vectors_tx(&tx, "node-00039").unwrap();
        tx.execute("INSERT INTO node(id,kind,created_at,payload_json,body_hash)VALUES('late','user','2026-11-07T00:00:00Z','{}','body')", [])?;
        tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('thread',1000,'late')", [])?;
        let work = Work::pending(
            Witness { node_id: "late".into(), revision: 0, body_hash: Some("body".into()), raw: None },
            "user".into(),
            Segment { start: 0, end: 1, text: "x".into() },
            "active",
            "explicit",
        );
        queue_job(&tx, &source, &work)?;
        let job = job_id_for(&tx, &source, &work)?.unwrap();
        store::adopt_vector_tx(&tx, &source, &job, &work, &adopted)?;
        let row = tx.query_row("SELECT vec_row_id FROM embedding_chunk WHERE source_node_id='late'", [], |r| r.get(0))?;
        tx.commit()?;
        Ok(row)
    }).await.unwrap();
    assert_eq!(row, 41, "above the cursor rather than the freed row 40");
    let target = f.record().await.unwrap().target.unwrap();
    assert_eq!(f.count("SELECT count(*) FROM embedding_chunk WHERE space_id=?1 AND source_node_id='node-00039'", &target).await, 0, "deletion reaches the candidate");
    f.drain().await;
    let active = f.active().await;
    assert_eq!(active.id, target);
    assert_eq!(f.count(CHUNKS, &active.id).await, 40);
    let hits = f.rank(&active, &added, 1).await;
    assert_eq!(hits[0].node_id, "late");
    assert_eq!(
        f.rank(&active, &vectors[39], 40)
            .await
            .iter()
            .filter(|h| h.node_id == "node-00039")
            .count(),
        0
    );
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn unfinished_jobs_follow_the_switch_and_gc_waits_for_them() {
    let f = fixture(clustered(3, 4), vec![1.0; 4]).await;
    let source = f.source.id.clone();
    let seeded = source.clone();
    f.db.call(move |db| {
        for (id, state, span) in [("pending", "pending", 10), ("failed", "failed", 11), ("split", "split", 12), ("running", "running", 13)] {
            let watermark = json!({"witness":{"node_id":"node-00000","revision":0,"body_hash":"body","raw":null},"group":"active","authorization":"explicit"}).to_string();
            db.execute("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,authorized_watermark)VALUES(?1,?2,'node-00000',0,'canonical','body',?3,?3,?4,?5)", params![id, seeded, span, state, watermark])?;
            db.execute("INSERT INTO app_meta(key,value)VALUES(?1,'saved input')", [IndexKeys::input(id)])?;
        }
        Ok(())
    }).await.unwrap();
    f.start().await;
    f.drain().await;
    // A claim in flight may still add a vector: no switch until it settles.
    assert_eq!(f.active().await.id, source);
    assert_eq!(f.record().await.unwrap().phase, COPY);
    f.db.call(|db| {
        db.execute(
            "UPDATE embedding_job SET state='failed' WHERE id='running'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    // The switch happens, then GC must not collect the old space's unfinished jobs.
    assert!(f.service.shrink_unit().await.unwrap());
    assert_eq!(f.record().await.unwrap().phase, SWEEP);
    let active = f.active().await.id;
    assert_ne!(active, source);
    assert_eq!(f.db.call(collect_inactive_spaces).await.unwrap(), 0);
    assert!(f.space(&source).await.is_some());
    f.drain().await;
    assert!(f.record().await.is_none());
    let moved = f.count("SELECT count(*) FROM embedding_job WHERE space_id=?1 AND id IN ('pending','failed','split','running')", &active).await;
    assert_eq!(moved, 4);
    let inputs =
        f.db.call(|db| {
            Ok(db.query_row(
                "SELECT count(*) FROM app_meta WHERE key LIKE 'index.input.%'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        inputs, 4,
        "saved inputs stay valid; nothing is re-embedded by the move"
    );
    assert_eq!(f.db.call(collect_inactive_spaces).await.unwrap(), 1);
    assert!(f.space(&source).await.is_none());
    // Logical accounting follows one byte per element, and the old float bytes left.
    let (counter, oracle): (i64, i64) = f.db.call(|db| {
        let counter = db.query_row("SELECT logical_bytes FROM quota_counter WHERE category='logical.index'", [], |r| r.get(0))?;
        let oracle = db.query_row("SELECT (SELECT COALESCE(SUM(s.dimension*(CASE WHEN s.int8_scale IS NULL THEN 4 ELSE 1 END)),0) FROM embedding_chunk c JOIN index_space s ON s.id=c.space_id)+(SELECT COALESCE(SUM(octet_length(value)),0) FROM app_meta WHERE key LIKE 'index.%')", [], |r| r.get(0))?;
        Ok((counter, oracle))
    }).await.unwrap();
    assert_eq!(counter, oracle);
    let status = f.service.status(Default::default()).await.unwrap();
    let space = status.spaces.iter().find(|s| s.space_id == active).unwrap();
    assert_eq!(space.vector_payload_bytes, 3 * 4);
    assert!(space.int8_scale.is_some());
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn paused_source_stays_paused_after_the_switch() {
    let f = fixture(clustered(3, 4), vec![1.0; 4]).await;
    let source = f.source.id.clone();
    f.db.call(move |db| {
        db.execute(
            "UPDATE index_space SET state='paused' WHERE id=?1",
            [source],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    f.start().await;
    f.drain().await;
    let active = f.active().await;
    assert!(active.int8_scale.is_some());
    assert_eq!(active.state, "paused");
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancel_refusals_and_a_later_space_choice_end_the_record() {
    let f = fixture(clustered(40, 4), vec![1.0; 4]).await;
    let dry = IndexShrinkArgs {
        dry_run: true,
        cancel: false,
    };
    let cancel = IndexShrinkArgs {
        dry_run: false,
        cancel: true,
    };
    assert!(
        f.service.shrink(cancel.clone()).await.is_err(),
        "nothing to cancel"
    );
    assert!(
        f.service
            .shrink(IndexShrinkArgs {
                dry_run: true,
                cancel: true
            })
            .await
            .is_err()
    );
    // Cancel keeps the old space and retires the partial candidate.
    f.start().await;
    while f.record().await.unwrap().phase != COPY {
        assert!(f.service.shrink_unit().await.unwrap());
    }
    assert_eq!(
        f.service.shrink(Default::default()).await.unwrap()["state"],
        "running"
    );
    assert!(f.service.shrink_unit().await.unwrap());
    let target = f.record().await.unwrap().target.unwrap();
    assert_eq!(f.service.shrink(cancel).await.unwrap()["cancelled"], true);
    assert!(f.record().await.is_none());
    assert_eq!(f.active().await.id, f.source.id);
    assert_eq!(f.space(&target).await.unwrap().state, "inactive");
    assert_eq!(f.db.call(collect_inactive_spaces).await.unwrap(), 1);
    // A leftover rebuild marker, for example of a failed rebuild, never blocks it.
    f.db.call(|db| {
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES('index.rebuild.p','g')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.service.shrink(dry.clone()).await.unwrap()["state"],
        "ready"
    );
    // The source stops being active: the conversion abandons itself.
    f.start().await;
    let source = f.source.id.clone();
    f.db.call(move |db| {
        db.execute(
            "UPDATE index_space SET state='inactive' WHERE id=?1",
            [source],
        )?;
        db.execute("DELETE FROM app_meta WHERE key='index.active_space'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    f.drain().await;
    assert!(f.record().await.is_none());
    assert_eq!(
        f.service.shrink(dry.clone()).await.unwrap()["state"],
        "no_active_index"
    );
    assert!(f.service.shrink(Default::default()).await.is_err());
    f.db.shutdown().await.unwrap();
    // Already int8 and empty report instead of converting.
    let f = fixture(clustered(2, 4), vec![1.0; 4]).await;
    f.start().await;
    f.drain().await;
    assert_eq!(
        f.service.shrink(dry.clone()).await.unwrap()["state"],
        "already_int8"
    );
    assert!(f.service.shrink(Default::default()).await.is_err());
    f.db.shutdown().await.unwrap();
    let f = fixture(Vec::new(), vec![1.0; 2]).await;
    assert_eq!(f.service.shrink(dry).await.unwrap()["state"], "empty");
    assert!(f.service.shrink(Default::default()).await.is_err());
    f.db.shutdown().await.unwrap();
}

fn spare_input_job(db: &Connection, id: &str, space: &str, state: &str, span: i64) {
    let watermark = json!({"witness":{"node_id":"node-00000","revision":0,"body_hash":"body","raw":null},"group":"active","authorization":"explicit"}).to_string();
    db.execute("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,authorized_watermark)VALUES(?1,?2,'node-00000',0,'canonical','body',?3,?3,?4,?5)", params![id, space, span, state, watermark]).unwrap();
    db.execute(
        "INSERT OR REPLACE INTO app_meta(key,value)VALUES(?1,'saved input')",
        [IndexKeys::input(id)],
    )
    .unwrap();
}

async fn logical_total(db: &DbWorker) -> u64 {
    db.call(|db| Ok(db.query_row("SELECT SUM(logical_bytes) FROM quota_counter WHERE category IN ('logical.history','logical.raw','logical.managed','logical.index')", [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap() as u64
}

/// A maintenance operation must not stop conversations: crossing the cap rolls
/// its page back, saves only its own failure, and pauses or discards nothing.
#[tokio::test]
async fn reaching_the_logical_cap_fails_only_the_conversion() {
    let f = fixture(clustered(100, 64), vec![1.0; 64]).await;
    let source = f.source.id.clone();
    let seeded = source.clone();
    f.db.call(move |db| {
        spare_input_job(db, "kept", &seeded, "failed", 50);
        Ok(())
    })
    .await
    .unwrap();
    let before = logical_total(&f.db).await;
    // Room for the first page and the metadata, not for the whole conversion.
    let mut limited = (*f.config).clone();
    limited.limits.data_max_bytes = before + 3_500;
    let limited = Arc::new(limited);
    let service = IndexService::new(f.db.clone(), limited.clone());
    service.shrink(Default::default()).await.unwrap();
    while service.shrink_unit().await.unwrap() {}
    let failed = f.record().await.unwrap();
    assert_eq!(failed.error.as_ref().unwrap()["code"], "storage_limit");
    assert_eq!(failed.target, None, "the partial candidate was given back");
    assert_eq!(f.active().await.id, source);
    let collected = f.db.call(collect_inactive_spaces).await.unwrap();
    assert_eq!(collected, 1, "a partial candidate existed and was retired");
    let after = logical_total(&f.db).await;
    assert!(after < limited.limits.data_max_bytes, "{after}");
    let check = limited.clone();
    let (pauses, job, input, capacity) = f.db.call(move |db| {
        Ok((
            db.query_row("SELECT count(*) FROM app_meta WHERE key>='index.pause.' AND key<'index.pause/'", [], |r| r.get::<_, i64>(0))?,
            db.query_row("SELECT count(*) FROM embedding_job WHERE id='kept'", [], |r| r.get::<_, i64>(0))?,
            meta(db, &IndexKeys::input("kept"))?.is_some(),
            crate::gc::require_capacity_tx(db, &check).is_ok(),
        ))
    }).await.unwrap();
    assert_eq!((pauses, job, input, capacity), (0, 1, true, true));
    // The real worker round leaves all of that as it is.
    service.process_one(CancellationToken::new()).await.unwrap();
    let pauses =
        f.db.call(|db| {
            Ok(db.query_row(
                "SELECT count(*) FROM app_meta WHERE key>='index.pause.' AND key<'index.pause/'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(pauses, 0);
    assert_eq!(
        f.count(
            "SELECT count(*) FROM embedding_job WHERE space_id=?1 AND id='kept'",
            &source
        )
        .await,
        1
    );
    // With room again, an explicit run starts over and completes.
    assert_eq!(
        f.service.shrink(Default::default()).await.unwrap()["state"],
        "resumed"
    );
    f.drain().await;
    assert_ne!(f.active().await.id, source);
    assert_eq!(f.count(CHUNKS, &f.active().await.id).await, 100);
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failed_run_is_released_when_the_source_stops_being_active() {
    let f = fixture(clustered(40, 8), vec![1.0; 8]).await;
    let source = f.source.id.clone();
    let trigger = source.clone();
    f.db.call(move |db| {
        db.execute_batch(&format!("CREATE TRIGGER refuse_copy BEFORE INSERT ON embedding_chunk WHEN NEW.vec_row_id=35 AND NEW.space_id<>'{trigger}' BEGIN SELECT RAISE(ABORT,'synthetic'); END;"))?;
        Ok(())
    }).await.unwrap();
    f.start().await;
    f.drain().await;
    let failed = f.record().await.unwrap();
    assert!(failed.error.is_some());
    let target = failed.target.unwrap();
    assert!(
        !f.service.shrink_unit().await.unwrap(),
        "still waiting for a choice"
    );
    // A rebuild candidate activates: source inactive, another space active.
    let moved = source.clone();
    f.db.call(move |db| {
        db.execute(
            "UPDATE index_space SET state='inactive' WHERE id=?1",
            [moved],
        )?;
        db.execute("DELETE FROM app_meta WHERE key='index.active_space'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(f.service.shrink_unit().await.unwrap());
    assert!(f.record().await.is_none());
    assert_eq!(f.space(&target).await.unwrap().state, "inactive");
    assert_eq!(
        f.db.call(collect_inactive_spaces).await.unwrap(),
        2,
        "source and candidate"
    );
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn sweep_drops_a_job_the_new_space_already_holds_and_survives_cancel_and_reopen() {
    let f = fixture(clustered(3, 4), vec![1.0; 4]).await;
    let source = f.source.id.clone();
    let seeded = source.clone();
    f.db.call(move |db| {
        spare_input_job(db, "duplicate", &seeded, "pending", 60);
        spare_input_job(db, "moved", &seeded, "failed", 61);
        Ok(())
    })
    .await
    .unwrap();
    f.start().await;
    while f.record().await.unwrap().phase != SWEEP {
        assert!(f.service.shrink_unit().await.unwrap());
    }
    let target = f.active().await.id;
    // The new space queued the same segment between the switch and the sweep.
    let queued = target.clone();
    f.db.call(move |db| {
        spare_input_job(db, "newer", &queued, "pending", 60);
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        f.service
            .shrink(IndexShrinkArgs {
                dry_run: false,
                cancel: true
            })
            .await
            .is_err()
    );
    let Fixture {
        temp, db, config, ..
    } = f;
    let path = db.path().to_path_buf();
    db.shutdown().await.unwrap();
    let db = DbWorker::open(path).await.unwrap();
    let service = IndexService::new(db.clone(), config);
    while service.shrink_unit().await.unwrap() {}
    let (ids, input): (Vec<String>, bool) = db.call(move |db| {
        let mut q = db.prepare("SELECT id FROM embedding_job WHERE space_id=?1 AND id IN ('duplicate','moved','newer') ORDER BY id")?;
        let ids = q.query_map([&target], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok((ids, meta(db, &IndexKeys::input("duplicate"))?.is_some()))
    }).await.unwrap();
    assert_eq!(ids, ["moved", "newer"]);
    assert!(!input, "the dropped job's input goes with it");
    assert_eq!(
        db.call(|db| read_record(&*db))
            .await
            .unwrap()
            .map(|r| r.phase),
        None
    );
    db.shutdown().await.unwrap();
    drop(temp);
}

#[tokio::test]
async fn resume_during_the_scale_pass_keeps_its_maximum() {
    let mut vectors = clustered(100, 16);
    vectors[3] = unit_vector((0..16).map(|i| if i == 0 { 1.0 } else { 0.0 }));
    let f = fixture(vectors, vec![1.0; 16]).await;
    f.start().await;
    assert!(f.service.shrink_unit().await.unwrap());
    let saved = f.record().await.unwrap();
    assert_eq!((saved.phase.as_str(), saved.max_abs), (SCALE, 1.0));
    let Fixture {
        temp, db, config, ..
    } = f;
    let path = db.path().to_path_buf();
    db.shutdown().await.unwrap();
    let db = DbWorker::open(path).await.unwrap();
    let service = IndexService::new(db.clone(), config);
    while service.shrink_unit().await.unwrap() {}
    let scale = db
        .call(|db| {
            let id = meta(db, IndexKeys::ACTIVE_SPACE)?.unwrap();
            Ok(load_space(db, &id)?.unwrap().int8_scale)
        })
        .await
        .unwrap();
    assert_eq!(scale, Some(127.0));
    db.shutdown().await.unwrap();
    drop(temp);
}

#[tokio::test]
async fn history_backed_rag_ranks_an_int8_space() {
    let vectors = clustered(20, 8);
    let f = fixture(vectors.clone(), vec![1.0; 8]).await;
    f.start().await;
    f.drain().await;
    let query = vectors[5].clone();
    let hits =
        f.db.call(move |db| {
            let descriptor = rag_space(db).unwrap().unwrap();
            Ok(rag_hits(db, &descriptor, &query, 1, None).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(hits[0].node_id, "node-00005");
    assert_eq!(hits[0].distance, 0.0);
    f.db.shutdown().await.unwrap();
}

/// Past the step budget with a foreground request waiting, a unit rolls back and
/// is deferred. That interruption is not a failure and must save no error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_budget_interrupt_saves_no_error_and_the_unit_runs_again() {
    let f = fixture(clustered(10, 4), vec![1.0; 4]).await;
    f.start().await;
    while f.record().await.unwrap().phase != COPY {
        assert!(f.service.shrink_unit().await.unwrap());
    }
    let source = f.source.id.clone();
    // Far more VM steps than the budget in the first copied row.
    f.db.call(move |db| {
        db.execute_batch(&format!("CREATE TRIGGER slow_copy BEFORE INSERT ON embedding_chunk WHEN NEW.space_id<>'{source}' BEGIN SELECT (WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<3000000) SELECT count(*) FROM n); END;"))?;
        Ok(())
    }).await.unwrap();
    let service = f.service.clone();
    let unit = tokio::spawn(async move { service.shrink_unit().await.unwrap() });
    // This foreground request waits behind the running unit.
    while !unit.is_finished() {
        f.db.call(|_| Ok(())).await.unwrap();
    }
    assert!(unit.await.unwrap(), "deferred units come back");
    let record = f.record().await.unwrap();
    assert!(record.error.is_none(), "{record:?}");
    assert_eq!(record.cursor, 0, "the page was rolled back");
    f.db.call(|db| {
        db.execute_batch("DROP TRIGGER slow_copy")?;
        Ok(())
    })
    .await
    .unwrap();
    f.drain().await;
    assert_eq!(f.count(CHUNKS, &f.active().await.id).await, 10);
    f.db.shutdown().await.unwrap();
    assert!(
        f.db.take_background_steps()
            .iter()
            .any(|s| s.0.contains("shrink") && s.2),
        "the interruption really happened"
    );
}

/// The real path: `Control` dispatches the command and the started workers convert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_dispatch_and_started_workers_convert_without_any_api_call() {
    use crate::protocol::{Command, Request};
    let root = tempfile::tempdir().unwrap();
    let (url, requests) = embedding_server(vec![1.0; 8]).await;
    let config_path = root.path().join("config.toml");
    std::fs::write(&config_path, format!(
        "default_connection=\"fixture\"\ndefault_model=\"synthetic\"\n[connections.fixture]\nbase_url={url:?}\n[embedding]\nconnection=\"fixture\"\nmodel=\"synthetic\"\n"
    )).unwrap();
    let owner = crate::app::DataOwner::start(
        Some(config_path),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let config = owner.loaded.config.clone();
    let source = owner.db.call(move |db| {
        let tx = db.transaction()?;
        let identity = identity::EmbeddingIdentity::configured(&config, identity::profile_key(&config).unwrap());
        let space = store::ensure_space(&tx, &identity, "active", 8)?;
        tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread','/','/a','fixture','synthetic','now');")?;
        seed_rows(&tx, &space, 0, &clustered(100, 8));
        tx.commit()?;
        Ok(space)
    }).await.unwrap();
    let control = crate::control::Control::new(&owner, root.path().into());
    control.start().await.unwrap();
    let send = |n: &'static str, command: Command| {
        let control = control.clone();
        let id = format!("{}:{n}", owner.instance_id);
        async move { control.request(Request::new(id, command)).await }
    };
    let preview = send(
        "preview",
        Command::IndexShrink(IndexShrinkArgs {
            dry_run: true,
            cancel: false,
        }),
    )
    .await;
    assert!(preview.ok, "{:?}", preview.error);
    let started = send("start", Command::IndexShrink(Default::default())).await;
    assert!(started.ok, "{:?}", started.error);
    let reader = owner.db.clone();
    let source_id = source.id.clone();
    crate::test_watchdog::watchdog("shrink through started workers", async move {
        loop {
            let id = source_id.clone();
            let done = reader
                .call(move |db| {
                    Ok(read_record(&*db)?.is_none()
                        && meta(db, IndexKeys::ACTIVE_SPACE)?.as_deref() != Some(id.as_str()))
                })
                .await
                .unwrap();
            if done {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let status = send("status", Command::IndexStatus(Default::default())).await;
    assert!(status.ok, "{:?}", status.error);
    let value = status.result.unwrap();
    assert!(value.get("shrink").is_none(), "{value}");
    let selected = value["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["selected"] == true)
        .unwrap();
    assert!(selected["int8_scale"].as_f64().unwrap() > 0.0);
    assert_eq!(selected["segments"], 100);
    assert_eq!(selected["vector_payload_bytes"], 800);
    assert_eq!(requests.load(Ordering::SeqCst), 0, "no embedding API call");
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

/// Rule 1 of the requester contract, at the dimension the real data uses.
#[tokio::test]
async fn every_unit_stays_within_the_background_step_budget_at_3072_dimensions() {
    let f = fixture(clustered(100, 3072), vec![1.0; 3072]).await;
    f.start().await;
    let units = f.drain().await;
    assert!(units > 6, "{units}");
    assert_eq!(f.count(CHUNKS, &f.active().await.id).await, 100);
    f.db.shutdown().await.unwrap();
    let mut max = 0;
    for (kind, steps, deferred) in f.db.take_background_steps() {
        assert!(!deferred, "{kind}");
        if kind.contains("shrink") {
            max = max.max(steps);
        }
    }
    println!("shrink unit max VM steps at 3072 dimensions: {max}");
    assert!(max > 0 && max <= BACKGROUND_STEP_BUDGET, "{max}");
}

/// Manual measurement on a real vector file; never touches a real database.
/// `SHELLBOY_SHRINK_VECTORS` is a raw little-endian f32 file (a copy under a
/// temporary directory); `SHELLBOY_SHRINK_DIMENSION` its dimension. Prints facts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual measurement: needs SHELLBOY_SHRINK_VECTORS and SHELLBOY_SHRINK_DIMENSION"]
async fn measure_conversion_on_a_vector_file() {
    use std::time::{Duration, Instant};
    let dimension: usize = std::env::var("SHELLBOY_SHRINK_DIMENSION")
        .unwrap()
        .parse()
        .unwrap();
    let bytes = std::fs::read(std::env::var("SHELLBOY_SHRINK_VECTORS").unwrap()).unwrap();
    let vectors: Vec<Vec<f32>> = bytes
        .chunks_exact(dimension * 4)
        .map(|row| {
            row.as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect()
        })
        .collect();
    drop(bytes);
    let count = vectors.len();
    let queries: Vec<Vec<f32>> = (0..5).map(|i| vectors[i * count / 5].clone()).collect();
    let started = Instant::now();
    let f = fixture(vectors, vec![1.0; dimension]).await;
    println!("seeded {count} vectors in {:?}", started.elapsed());
    let path = f.db.path().to_path_buf();
    crate::storage::register_vec().unwrap();
    let measure = |space: &Space, label: &str, queries: &[Vec<f32>]| {
        let open = || {
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
        };
        let db = open();
        let stored: i64 = db
            .query_row(
                &format!(
                    "SELECT sum(length(vectors)) FROM {}_vector_chunks00",
                    space.table
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        println!("{label}: bytes one KNN scan reads (vector chunks) {stored}");
        let ids: String = db
            .prepare("SELECT json_group_array(vec_row_id) FROM embedding_chunk WHERE space_id=?1")
            .unwrap()
            .query_row([&space.id], |r| r.get(0))
            .unwrap();
        for query in queries {
            let db = open();
            let encoded = space.encode_vector(query).unwrap();
            // The vec0 scan alone: the production statement shape without joins.
            let sql = format!(
                "SELECT rowid,distance FROM {} WHERE embedding MATCH {} AND k=4096 AND rowid IN (SELECT value FROM json_each(?2)) ORDER BY distance",
                space.table,
                space.vector_sql("?1")
            );
            let t = Instant::now();
            let rows = db
                .prepare(&sql)
                .unwrap()
                .query_map(params![encoded, ids], |r| r.get::<_, i64>(0))
                .unwrap()
                .count();
            let knn = t.elapsed();
            let t = Instant::now();
            let hits = search::filtered_hits(
                &db,
                space,
                &encoded,
                &HistorySearchArgs {
                    query: "m".into(),
                    limit: 10,
                    ..Default::default()
                },
            )
            .unwrap();
            println!(
                "{label}: knn scan {knn:?} ({rows} rows); whole ranking {:?} ({} hits)",
                t.elapsed(),
                hits.len()
            );
        }
    };
    measure(&f.source, "float32", &queries);
    let page_count = |db: &DbWorker| {
        let db = db.clone();
        async move {
            db.call(|db| Ok(db.query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))?))
                .await
                .unwrap()
        }
    };
    let before = page_count(&f.db).await;
    // Foreground writes during the conversion: how long does the writer make them wait?
    let (stop, writer) = (CancellationToken::new(), f.db.clone());
    let stopped = stop.clone();
    let probe = tokio::spawn(async move {
        let mut samples = Vec::new();
        while !stopped.is_cancelled() {
            let t = Instant::now();
            writer
                .call(|db| {
                    db.execute(
                        "INSERT OR REPLACE INTO app_meta(key,value)VALUES('probe','x')",
                        [],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            samples.push(t.elapsed());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        samples
    });
    f.start().await;
    let mut phases: BTreeMap<String, Vec<Duration>> = BTreeMap::new();
    loop {
        let phase = f.record().await.map_or("end".to_owned(), |r| r.phase);
        let t = Instant::now();
        let more = f.service.shrink_unit().await.unwrap();
        if !more {
            break;
        }
        phases.entry(phase).or_default().push(t.elapsed());
    }
    stop.cancel();
    let mut write_waits = probe.await.unwrap();
    for (phase, mut units) in phases {
        units.sort();
        println!(
            "phase {phase}: units={} total={:?} median={:?} p99={:?} max={:?}",
            units.len(),
            units.iter().sum::<Duration>(),
            units[units.len() / 2],
            units[units.len() * 99 / 100],
            units[units.len() - 1]
        );
    }
    write_waits.sort();
    println!(
        "foreground write during conversion: samples={} median={:?} p99={:?} max={:?}",
        write_waits.len(),
        write_waits[write_waits.len() / 2],
        write_waits[write_waits.len() * 99 / 100],
        write_waits[write_waits.len() - 1]
    );
    let active = f.active().await;
    println!(
        "scale={:?} state={} chunks={}",
        active.int8_scale,
        active.state,
        f.count(CHUNKS, &active.id).await
    );
    let after = page_count(&f.db).await;
    println!("page_count before={before} after={after}");
    let t = Instant::now();
    let collected = f.db.call(collect_inactive_spaces).await.unwrap();
    println!("collected {collected} old space in {:?}", t.elapsed());
    measure(&active, "int8 (old space collected)", &queries);
    f.db.shutdown().await.unwrap();
    let steps = f.db.take_background_steps();
    let max = steps
        .iter()
        .filter(|s| s.0.contains("shrink"))
        .map(|s| s.1)
        .max();
    println!("max VM steps per unit {max:?} (budget {BACKGROUND_STEP_BUDGET})");
}
