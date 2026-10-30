//! Opt-in synthetic component measurements, never a normal CI acceptance gate.
//! Run serially with --ignored --nocapture --test-threads=1 (see performance.md).
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use shellboy::{
    config::Config,
    history::History,
    protocol::HistoryListArgs,
    storage::DbWorker,
    ui::{UiNode, UiState, UiUpdate, draw},
};
#[cfg(unix)]
use shellboy::{
    control::output::{OutputCursor, ShellOutputReader, StreamSelection},
    protocol::ShellReadArgs,
};
use std::time::{Duration, Instant};

fn report(name: &str, samples: &[Duration], extra: serde_json::Value) {
    let mut us: Vec<f64> = samples
        .iter()
        .map(|d| d.as_secs_f64() * 1_000_000.0)
        .collect();
    us.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        us[((us.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(us.len() - 1)]
    };
    println!(
        "PERF {}",
        serde_json::json!({"name":name,"samples":us.len(),"p50_us":percentile(0.50),"p95_us":percentile(0.95),"max_us":us.last(),"extra":extra})
    );
}

async fn seed_history(db: &DbWorker, count: usize) {
    db.call(|db| { db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('history','/','/AGENTS.md','fixture','fixture','now')", [])?; Ok(()) }).await.unwrap();
    // Fixed-size short DB jobs; no complete graph/body collection in memory.
    for start in (0..count).step_by(1000) {
        db.call(move |db| {
            let tx=db.transaction()?;
            {
                let mut node=tx.prepare("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'assistant','now',?2,?3)")?;
                let mut path=tx.prepare("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('history',?1,?2)")?;
                for i in start..(start+1000).min(count) {
                    let id=format!("n{i:06}");
                    let payload=serde_json::json!({"text":format!("synthetic node {i}: {}", "日本語 bounded payload ".repeat(4))}).to_string();
                    node.execute(rusqlite::params![id,payload,payload.len() as i64])?;
                    path.execute(rusqlite::params![i as i64,id])?;
                }
            }
            tx.commit()?; Ok(())
        }).await.unwrap();
        tokio::task::yield_now().await;
    }
    db.call(move |db| {
        db.execute(
            "UPDATE thread SET cursor_node_id=?1 WHERE id='history'",
            [format!("n{:06}", count - 1)],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "synthetic 10k/100k fixture and repeated timing; run explicitly in isolation"]
async fn performance_history_startup_and_pages() {
    for count in [10_000, 100_000] {
        if let Ok(selected) = std::env::var("SHELLBOY_PERF_HISTORY_NODES") {
            assert!(matches!(selected.as_str(), "10000" | "100000"));
            if selected.parse::<usize>().unwrap() != count {
                continue;
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("shellboy.db");
        let db = DbWorker::open(&path).await.unwrap();
        let start = Instant::now();
        seed_history(&db, count).await;
        report(
            "history_fixture_insert",
            &[start.elapsed()],
            serde_json::json!({"nodes":count,"batch":1000}),
        );
        db.shutdown().await.unwrap();
        let mut opens = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            let db = DbWorker::open(&path).await.unwrap();
            opens.push(start.elapsed());
            db.shutdown().await.unwrap();
        }
        report(
            "dbworker_existing_open",
            &opens,
            serde_json::json!({"nodes":count,"scope":"DbWorker open/migration validation only; warm OS cache; no DataOwner recovery"}),
        );
        let config_path = root.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mut owner_opens = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            let owner = shellboy::app::DataOwner::start(
                Some(config_path.clone()),
                Some(root.path().into()),
                root.path().into(),
            )
            .await
            .unwrap();
            owner_opens.push(start.elapsed());
            owner.shutdown().await.unwrap();
        }
        report(
            "dataowner_existing_start",
            &owner_opens,
            serde_json::json!({"nodes":count,"scope":"config load, owner lock, DbWorker open, recovery transaction, diagnostics startup; warm OS cache; no Control/IPC/TUI"}),
        );
        let db = DbWorker::open(&path).await.unwrap();
        let history = History::new(db.clone());
        let mut timings = Vec::new();
        let mut cursor = None;
        let mut max_returned = 0;
        for _ in 0..200 {
            let start = Instant::now();
            let page = history
                .list(HistoryListArgs {
                    excerpt_chars: shellboy::protocol::DEFAULT_HISTORY_EXCERPT_CHARS,
                    thread_id: "history".into(),
                    limit: 50,
                    cursor,
                    run_id: None,
                    since: None,
                    state: None,
                })
                .await
                .unwrap();
            timings.push(start.elapsed());
            max_returned = max_returned.max(page.items.len());
            assert!(page.items.len() <= 50);
            cursor = page.next_cursor;
            if cursor.is_none() {
                cursor = None;
            }
        }
        report(
            "history_list_page50",
            &timings,
            serde_json::json!({"nodes":count,"max_returned":max_returned,"requests":200,"scope":"real History::list, including count query, cursor and DB queue"}),
        );
        db.shutdown().await.unwrap();
    }
}

#[test]
#[ignore = "1000 TestBackend draws and state timings; no real terminal measurement"]
fn performance_ui_state_and_test_backend() {
    let mut state = UiState::new(&Config::default(), std::env::temp_dir());
    state.select_thread("fixture".into());
    let generation = state.view().generation;
    let body = "synthetic 日本語😀 output\n".repeat(128);
    state.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: "fixture".into(),
        generation,
        nodes: (0..50)
            .map(|i| UiNode {
                presentation: Default::default(),
                node_id: format!("ui{i}"),
                role: "assistant".into(),
                base_char_offset: 0,
                total_chars: body.chars().count(),
                text: body.clone(),
                reasoning_ranges: vec![],
            })
            .collect(),
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 0,
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert!(!state.dirty);
    let mut idle_draws = 0;
    let mut idle = Vec::new();
    for _ in 0..1000 {
        let start = Instant::now();
        if state.dirty {
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            idle_draws += 1;
        }
        idle.push(start.elapsed());
    }
    assert_eq!(idle_draws, 0);
    report(
        "ui_idle_dirty_gate",
        &idle,
        serde_json::json!({"redraws":idle_draws,"iterations":1000,"scope":"pure dirty check, no event poll/sleep/process CPU claim"}),
    );
    let mut input = Vec::new();
    let mut frames = Vec::new();
    for _ in 0..1000 {
        let start = Instant::now();
        state.event(Event::Key(KeyEvent::new(
            KeyCode::Char('日'),
            KeyModifiers::NONE,
        )));
        input.push(start.elapsed());
        let start = Instant::now();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        frames.push(start.elapsed());
        state.event(Event::Key(KeyEvent::new(
            KeyCode::Backspace,
            KeyModifiers::NONE,
        )));
    }
    report(
        "ui_state_key_dispatch",
        &input,
        serde_json::json!({"nodes":50,"retained_body_bytes":body.len()*50,"terminal_cells":"120x40","scope":"UiState::event synchronous dispatch only"}),
    );
    report(
        "ui_test_backend_draw",
        &frames,
        serde_json::json!({"nodes":50,"terminal_cells":"120x40","scope":"layout/render/TestBackend diff; no OS write/event queue/frame pacing"}),
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real Bash 16/64MiB raw output, SQLite persistence and decoder scans"]
async fn performance_shell_raw_backpressure_and_read() {
    use shellboy::{
        config::ShellKind,
        protocol::ShellToolArgs,
        shell::{ShellEvent, ShellSupervisor},
    };
    use std::{collections::BTreeMap, path::PathBuf};
    for mib in [16_u64, 64] {
        if let Ok(selected) = std::env::var("SHELLBOY_PERF_RAW_MIB") {
            assert!(matches!(selected.as_str(), "16" | "64"));
            if selected.parse::<u64>().unwrap() != mib {
                continue;
            }
        }
        let root = tempfile::tempdir().unwrap();
        let db = DbWorker::open(root.path().join("raw.sqlite3"))
            .await
            .unwrap();
        db.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('raw','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('raw-run','raw','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('raw-turn','raw','raw-run','running','now'); INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('raw-shell','raw-run','raw-turn','raw-op','running','/','{}','UTF-8','UTF-8','closed','now');")?;Ok(())}).await.unwrap();
        let mut config = Config::default();
        config.shell.kind = ShellKind::Bash;
        let args:ShellToolArgs=serde_json::from_value(serde_json::json!({"command":format!("head -c {} /dev/zero | tr '\\000' x",mib*1024*1024)})).unwrap();
        let start = Instant::now();
        let mut execution = ShellSupervisor::new(&config)
            .spawn(
                "raw-shell".into(),
                PathBuf::from("/"),
                args,
                BTreeMap::new(),
            )
            .await
            .unwrap();
        // Exceeds the configured 8MiB producer budget: stalled consumer must
        // block completion; bytes remain under the runtime's semaphore permits.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(execution.completion.borrow().is_none());
        let mut probes = Vec::new();
        for _ in 0..25 {
            let now = Instant::now();
            db.call(|db| Ok(db.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
                .await
                .unwrap();
            probes.push(now.elapsed());
        }
        report(
            "db_probe_while_shell_consumer_stalled",
            &probes,
            serde_json::json!({"mib":mib,"consumer_stall_ms":150,"shell_complete_at_probe":false}),
        );
        let mut bytes = 0_u64;
        let mut chunks = 0_u64;
        let mut writes = Vec::new();
        while let Some(ShellEvent::Output(chunk)) = execution.events.recv().await {
            let raw = chunk.bytes.clone();
            let sequence = chunk.sequence;
            let offset = bytes;
            bytes += raw.len() as u64;
            chunks += 1;
            let now = Instant::now();
            db.call(move|db|{let tx=db.transaction()?;tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('raw-shell',?1,'stdout',?2,?3,'now')",rusqlite::params![sequence as i64,offset as i64,raw])?;tx.execute("UPDATE shell_execution SET saved_sequence=?1 WHERE id='raw-shell'",[sequence as i64+1])?;tx.commit()?;Ok(())}).await.unwrap();
            writes.push(now.elapsed());
            drop(chunk);
        }
        while execution.completion.borrow().is_none() {
            execution.completion.changed().await.unwrap();
        }
        let completion = execution.completion.borrow().clone().unwrap();
        assert!(completion.output_complete);
        assert_eq!(bytes, mib * 1024 * 1024);
        report(
            "shell_generate_and_persist",
            &[start.elapsed()],
            serde_json::json!({"mib":mib,"chunks":chunks,"raw_bytes":bytes,"includes_stall":true,"queue_budget_bytes":config.io.output_buffer_bytes}),
        );
        report(
            "raw_chunk_save",
            &writes,
            serde_json::json!({"mib":mib,"chunk_bytes_max":8192,"scope":"DB queue plus real chunk transaction/commit"}),
        );
        db.call(|db|{db.execute("UPDATE shell_execution SET state='exited',exit_code=0,output_complete=1,ended_at='now' WHERE id='raw-shell'",[])?;Ok(())}).await.unwrap();
        let reader = ShellOutputReader::new(db.clone(), "benchmark");
        let mut total = 0_u64;
        let mut pages = Vec::new();
        let now = Instant::now();
        let mut stream = reader.stream("raw-shell").await.unwrap();
        while let Some(page) = {
            let start = Instant::now();
            let page = stream.next_page(65536).await.unwrap();
            if page.is_some() {
                pages.push(start.elapsed());
            }
            page
        } {
            total += page.raw_bytes;
            assert!(page.text.len() <= 65536);
        }
        assert_eq!(total, bytes);
        report(
            "raw_stream_scan",
            &[now.elapsed()],
            serde_json::json!({"mib":mib,"pages":pages.len(),"raw_bytes":total,"scope":"one-pass independent decoder session; returned text dropped every page"}),
        );
        report("raw_stream_page64k", &pages, serde_json::json!({"mib":mib}));
        for (label, sequence) in [
            ("start", 0),
            ("middle", chunks / 2),
            ("last_chunk", chunks - 1),
        ] {
            let mut timings = Vec::new();
            for _ in 0..3 {
                let cursor = OutputCursor {
                    version: 1,
                    execution_id: "raw-shell".into(),
                    sequence,
                    byte_offset: 0,
                    retention_generation: 0,
                    encoding: "UTF-8".into(),
                    streams: StreamSelection::Both,
                    eof_flushed: false,
                }
                .encode()
                .unwrap();
                let now = Instant::now();
                let page = reader
                    .read(ShellReadArgs {
                        execution_id: "raw-shell".into(),
                        cursor: Some(cursor),
                        encoding: None,
                        limit_bytes: 1,
                    })
                    .await
                    .unwrap();
                timings.push(now.elapsed());
                assert_eq!(page.raw_bytes, 1);
            }
            report(
                "raw_stateless_read1",
                &timings,
                serde_json::json!({"mib":mib,"position":label,"sequence":sequence,"scope":"prefix decoder reconstruction O(prefix); not constant-time seek"}),
            );
        }
        db.shutdown().await.unwrap();
    }
}

/// Local deterministic embeddings exercise the real query HTTP, IndexService,
/// DB worker and fixed sqlite-vec path without network-provider latency/cost.
#[tokio::test]
#[ignore = "1,000 real vec rows and 110 local embedding queries; explicit release measurement only"]
async fn performance_index_service_search_1000_sources() {
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use shellboy::{config::Connection, index::IndexService, protocol::IndexArgs};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_util::sync::CancellationToken;

    const SOURCES: usize = 1_000;
    const DIMENSION: usize = 1_536;
    const WARMUP: usize = 10;
    const SAMPLES: usize = 100;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let captured_calls = calls.clone();
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    let proxy = tokio::spawn(async move {
        loop {
            let (mut stream, _) = tokio::select! {
                _ = stopped.cancelled() => break,
                result = listener.accept() => result.unwrap(),
            };
            let mut bytes = Vec::new();
            let split = loop {
                let mut chunk = [0; 4096];
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(offset) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    break offset + 4;
                }
            };
            let length = String::from_utf8_lossy(&bytes[..split])
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap();
            while bytes.len() < split + length {
                let mut chunk = [0; 4096];
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            let request: Value = serde_json::from_slice(&bytes[split..split + length]).unwrap();
            let input = request["input"][0].as_str().unwrap();
            let coordinate: usize = input.split("item-").nth(1).unwrap()[..4].parse().unwrap();
            assert!(coordinate < SOURCES);
            let mut embedding = vec![0.0; DIMENSION];
            embedding[coordinate] = 1.0;
            captured_calls.fetch_add(1, Ordering::Relaxed);
            let body = json!({"data":[{"index":0,"embedding":embedding}]}).to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
            stream.write_all(body.as_bytes()).await.unwrap();
        }
    });
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("search.sqlite"))
        .await
        .unwrap();
    let mut config = Config::default();
    config.connections.insert(
        "fixture".into(),
        Connection {
            base_url,
            ..Connection::default()
        },
    );
    config.embedding.connection = Some("fixture".into());
    config.embedding.model = Some("synthetic-1536-one-hot".into());
    let service = IndexService::new(db.clone(), Arc::new(config));
    let ids: Vec<String> = (0..SOURCES)
        .map(|index| format!("item-{index:04}"))
        .collect();
    db.call(|db| {
        let tx = db.transaction()?;
        {
            let mut insert = tx.prepare("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,'user','2026-10-24T00:00:00Z',?2,?3,?4)")?;
            for index in 0..SOURCES {
                let id = format!("item-{index:04}");
                let payload = json!({"role":"user","content":[{"type":"input_text","text":format!("{id}: synthetic searchable source 日本語 bounded excerpt") }]}).to_string();
                let hash = Sha256::digest(payload.as_bytes()).iter().map(|byte|format!("{byte:02x}")).collect::<String>();
                insert.execute(rusqlite::params![id,payload,payload.len() as i64,hash])?;
            }
        }
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let preview = service.enqueue_new(&ids).await.unwrap();
    assert_eq!(preview.sources, SOURCES as u64);
    let mut settled = false;
    for _ in 0..SOURCES * 3 + 16 {
        if !service.process_one(CancellationToken::new()).await.unwrap() {
            settled = true;
            break;
        }
    }
    assert!(
        settled,
        "finite source preparation/embedding queue did not settle"
    );
    let status = service
        .status(IndexArgs {
            dimensions: None,
            scope: None,
            space_id: None,
            thread_ids: vec![],
            source_ids: vec![],
        })
        .await
        .unwrap();
    assert_eq!(status.done, SOURCES as u64);
    assert_eq!(calls.load(Ordering::Relaxed), SOURCES);
    for index in 0..WARMUP {
        let matches = service
            .search(&format!("item-{index:04}"), 10)
            .await
            .unwrap();
        assert_eq!(matches[0].node_id, format!("item-{index:04}"));
    }
    let mut timings = Vec::with_capacity(SAMPLES);
    for index in 0..SAMPLES {
        let target = format!("item-{:04}", index * 19 % SOURCES);
        let started = Instant::now();
        let matches = service.search(&target, 10).await.unwrap();
        timings.push(started.elapsed());
        assert_eq!(matches.len(), 10);
        assert_eq!(matches[0].node_id, target);
        assert!(matches[0].excerpt.contains(&target));
    }
    assert_eq!(calls.load(Ordering::Relaxed), SOURCES + WARMUP + SAMPLES);
    report(
        "index_service_search",
        &timings,
        json!({"sources":SOURCES,"chunks":status.done,"dimension":DIMENSION,"warmup":WARMUP,"limit":10,"embedding_calls":calls.load(Ordering::Relaxed),"scope":"real IndexService query embedding HTTP + JSON validation + DbWorker + sqlite-vec KNN + match/excerpt assembly; sequential, warm cache, synthetic one-hot vectors; no provider latency or ranking-quality claim","cache":{"body_bytes":32*1024*1024,"layout_bytes":16*1024*1024,"history_bytes":32*1024*1024,"attachment_base64_bytes":64*1024*1024},"ui_present":false}),
    );
    service.wait_idle().await.unwrap();
    db.shutdown().await.unwrap();
    stop.cancel();
    proxy.await.unwrap();
}

/// One accepted user node already exists outside the selected path, as in
/// driver::next. Roll back every observation so all samples adopt the same node
/// onto the identical selected path and edge set.
#[tokio::test]
#[ignore = "synthetic 1k/3k selected-path adoption timing; run explicitly in isolation"]
async fn performance_history_receipt_adoption() {
    for count in [1_000, 3_000] {
        let root = tempfile::tempdir().unwrap();
        let db = DbWorker::open(root.path().join("shellboy.db"))
            .await
            .unwrap();
        seed_history(&db, count).await;
        db.call(move |db| {
            let tx = db.transaction()?;
            for i in 1..count {
                tx.execute(
                    "INSERT INTO edge(src,dst) VALUES(?1,?2)",
                    rusqlite::params![format!("n{:06}", i - 1), format!("n{i:06}")],
                )?;
            }
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES('pending-user','user','now','{}',2)", [])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let mut timings = Vec::new();
        for _ in 0..5 {
            timings.push(db.call(move |db| {
                let tx = db.transaction()?;
                let before: (i64, i64) = tx.query_row(
                    "SELECT selection_revision,revision FROM thread WHERE id='history'",
                    [], |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let root_before: i64 = tx.query_row("SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'", [], |r| r.get(0))?;
                let started = Instant::now();
                shellboy::history::append_existing_nodes(&tx, "history", before.0 as u64, &[String::from("pending-user")], "adopted")?;
                let elapsed = started.elapsed();
                let state: (i64, i64, String, i64) = tx.query_row(
                    "SELECT selection_revision,revision,cursor_node_id,(SELECT COUNT(*) FROM thread_path WHERE thread_id=t.id) FROM thread t WHERE id='history'", [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?;
                assert_eq!(state, (before.0 + 1, before.1 + 1, "pending-user".into(), count as i64 + 1));
                let edges: i64 = tx.query_row("SELECT COUNT(*) FROM edge", [], |r| r.get(0))?;
                assert_eq!(edges, count as i64);
                let root_after: i64 = tx.query_row("SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'", [], |r| r.get(0))?;
                assert_eq!(root_after, root_before + 1);
                for ordinal in [0, count / 2, count - 1] {
                    let node: String = tx.query_row("SELECT node_id FROM thread_path WHERE thread_id='history' AND ordinal=?1", [ordinal as i64], |r| r.get(0))?;
                    assert_eq!(node, format!("n{ordinal:06}"));
                }
                tx.rollback()?;
                Ok(elapsed)
            }).await.unwrap());
        }
        report(
            "history_receipt_adoption",
            &timings,
            serde_json::json!({
                "path_nodes":count,"pending_nodes":1,"implementation":"append_existing_nodes",
                "scope":"driver-equivalent selected-tail adoption inside DbWorker; no receipt query, replay, HTTP, commit/fsync, or UI; fixed fixture rolled back after every sample",
                "sqlite_version":rusqlite::version(),"fixture":"forward linear DAG, one existing unselected user node"
            }),
        );
        db.shutdown().await.unwrap();
    }
}
