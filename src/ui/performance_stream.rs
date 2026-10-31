//! Opt-in same-source mock transport/renderer comparison. Not a real terminal.
use super::*;
use crate::{app::DataOwner, control::Control, frontend, ipc::LocalServer, protocol::*};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;
use std::{
    sync::{Arc, OnceLock, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};
const DELTAS: usize = 1024;
fn row(index: usize) -> String {
    format!("STREAM_EVENT_{index:04} {}\n", "字".repeat(20))
}
fn percentile(mut values: Vec<f64>, rank: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() as f64 * rank).ceil() as usize).saturating_sub(1)]
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in localhost SSE + real frontend/TestBackend load measurement; run alone in release"]
#[allow(clippy::assertions_on_constants)] // The opt-in ignored harness must compile in debug, but measures release only.
async fn stream_input_layout_and_db_fairness() {
    assert!(!cfg!(debug_assertions), "release measurement only");
    frontend::stream_metrics::reset();
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture-model'\n[connections.fixture]\nbase_url='http://{}/v1'\n",listener.local_addr().unwrap())).unwrap();
    let stamps = Arc::new(
        (0..DELTAS)
            .map(|_| OnceLock::<Instant>::new())
            .collect::<Vec<_>>(),
    );
    let started = Arc::new(Notify::new());
    let proxy_stamps = stamps.clone();
    let proxy_started = started.clone();
    let expected = (0..DELTAS).map(row).collect::<String>() + "STREAM_FINAL";
    let final_text = expected.clone();
    let proxy = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let split = loop {
            let mut bytes = [0; 4096];
            let n = socket.read(&mut bytes).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&bytes[..n]);
            if let Some(at) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let length: usize = String::from_utf8_lossy(&request[..split])
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|n| n.trim().parse().unwrap())
            })
            .unwrap();
        while request.len() < split + length {
            let mut bytes = [0; 4096];
            let n = socket.read(&mut bytes).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&bytes[..n]);
        }
        let packets = (0..DELTAS)
            .map(|index| {
                format!(
                    "data: {}\n\n",
                    json!({"type":"response.output_text.delta","delta":row(index)})
                )
            })
            .collect::<Vec<_>>();
        let final_event = format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":{"id":"stream-perf","status":"completed","output":[{"id":"message","type":"message","role":"assistant","content":[{"type":"output_text","text":final_text}]}]}})
        );
        let bytes = packets.iter().map(String::len).sum::<usize>() + final_event.len();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {bytes}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        proxy_started.notified().await;
        for (index, packet) in packets.iter().enumerate() {
            proxy_stamps[index].set(Instant::now()).unwrap();
            socket.write_all(packet.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        socket.write_all(final_event.as_bytes()).await.unwrap();
    });
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let observer = control.clone();
    let mut ui = UiState::new(&control.store.config, root.path().into());
    let (rpc, receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, rpc).await.unwrap();
    tokio::spawn(control.clone().serve(receive));
    let (send, receive) = tokio::sync::mpsc::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    ui.insert("synthetic stream", false);
    for action in ui.event(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
    )) {
        send.try_send(action).unwrap();
    }
    let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
    let period = Duration::from_secs_f64(1. / 30.);
    let mut next_frame = Instant::now();
    let mut redraw = true;
    let mut next_input = None;
    let mut input_samples = Vec::new();
    let mut pending_input: Option<(String, Instant)> = None;
    let mut lag = Vec::new();
    let mut db_samples = tokio::task::JoinSet::new();
    let mut metadata_jobs = tokio::task::JoinSet::new();
    let mut projected_samples = 0usize;
    let mut projected_sample_bytes = 0usize;
    let mut draw_ns = 0u128;
    let mut draws = 0usize;
    let mut last_index = None;
    let mut gaps = 0usize;
    let mut stream_ready = false;
    let mut proxy_released = false;
    let mut saved = false;
    let guard =
        crate::test_watchdog::Deadline::new("load fixture completes saved state and inputs");
    let start = Instant::now();
    while !(saved && input_samples.len() == 7) {
        assert!(
            !guard.expired(),
            "load fixture timeout; projected_samples={projected_samples}, inputs={}, saved={saved}",
            input_samples.len()
        );
        // Apply the production 32-update/4ms pump, including its one retention
        // pass. Observe only the resulting view; inspecting notifications here
        // would consume them before the production admission/retention path.
        ui.drain_updates(&incoming);
        redraw |= ui.dirty;
        for node in ui.view().nodes() {
            let text = &node.text;
            if let Some(at) = text.rfind("STREAM_EVENT_") {
                let index: usize = text[at + 13..at + 17].parse().unwrap();
                if last_index.is_none_or(|previous| index > previous) {
                    projected_samples += 1;
                    projected_sample_bytes += text.len();
                    if let Some(sent) = stamps[index].get() {
                        lag.push(sent.elapsed().as_secs_f64() * 1000.);
                    }
                    gaps += index.saturating_sub(last_index.map_or(0, |previous| previous + 1));
                    last_index = Some(index);
                    if !stream_ready {
                        stream_ready = true;
                        next_input = Some(Instant::now());
                    }
                }
            }
            if text.ends_with("STREAM_FINAL")
                && !ui.view().provisional_nodes.contains(&node.node_id)
            {
                saved = true;
                assert_eq!(node.total_chars, expected.chars().count());
            }
        }
        if !proxy_released && ui.view().thread_id.is_some() {
            proxy_released = true;
            started.notify_one();
        }
        for action in ui.take_deferred_actions() {
            if let Err(error) = send.try_send(action) {
                ui.action_rejected(&error.into_inner());
            }
        }
        if pending_input.is_none()
            && input_samples.len() < 7
            && next_input.is_some_and(|due| Instant::now() >= due)
        {
            let marker = format!("INPUT_{:02}", input_samples.len());
            let now = Instant::now();
            for action in ui.event(crossterm::event::Event::Paste(marker.clone())) {
                if let Err(error) = send.try_send(action) {
                    ui.action_rejected(&error.into_inner());
                }
            }
            pending_input = Some((marker, now));
            redraw = true;
            let c = observer.clone();
            db_samples.spawn(async move {
                let now = Instant::now();
                c.store
                    .database
                    .call(|db| {
                        let _: i64 = db.query_row(
                            "SELECT count(*) FROM model_attempt WHERE state='inflight'",
                            [],
                            |row| row.get(0),
                        )?;
                        Ok(())
                    })
                    .await
                    .unwrap();
                now.elapsed().as_secs_f64() * 1000.
            });
            if let Some(thread) = ui.view().thread_id.clone() {
                let c = observer.clone();
                let title = format!("metadata refresh {}", input_samples.len());
                metadata_jobs.spawn(async move {
                    let response = c
                        .request(Request::new(
                            format!("{}:{}", c.store.instance_id, uuid::Uuid::new_v4()),
                            Command::ThreadSet(ThreadSetArgs {
                                agents: None,
                                thread_id: Some(thread),
                                connection: None,
                                cwd: None,
                                model: None,
                                effort: None,
                                title: Some(title),
                            }),
                        ))
                        .await;
                    assert!(response.ok);
                });
            }
        }
        if redraw && Instant::now() >= next_frame {
            let now = Instant::now();
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
            draw_ns += now.elapsed().as_nanos();
            draws += 1;
            next_frame = Instant::now() + period;
            redraw = false;
            if let Some((marker, at)) = &pending_input {
                let visible = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                if visible.contains(marker) {
                    input_samples.push(at.elapsed().as_secs_f64() * 1000.);
                    pending_input = None;
                    next_input = Some(Instant::now() + Duration::from_millis(180));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    proxy.await.unwrap();
    while let Some(result) = metadata_jobs.join_next().await {
        result.unwrap();
    }
    let mut db_ms = Vec::new();
    while let Some(result) = db_samples.join_next().await {
        db_ms.push(result.unwrap());
    }
    let counters = &frontend::stream_metrics::SNAPSHOTS;
    println!(
        "PERF_STREAM {}",
        json!({"deltas":DELTAS,"source_bytes":expected.len(),"snapshots":counters.load(Ordering::Relaxed),"cloned_bytes":frontend::stream_metrics::CLONED_BYTES.load(Ordering::Relaxed),"post_pump_view_samples":projected_samples,"post_pump_sample_bytes":projected_sample_bytes,"markers_not_observed_between_pumps":gaps,"layout_builds":ui.layout_cache.builds,"draws":draws,"draw_total_ms":draw_ns as f64/1e6,"source_to_projected_view_ms":{"n":lag.len(),"p50":percentile(lag.clone(),0.5),"p95":percentile(lag,0.95)},"input_to_testbackend_ms":{"n":input_samples.len(),"samples":input_samples},"db_roundtrip_ms":{"n":db_ms.len(),"samples":db_ms},"display_db_gate_reads":frontend::stream_metrics::GATE_READS.load(Ordering::Relaxed),"display_db_gate_service_ns":frontend::stream_metrics::GATE_NANOS.load(Ordering::Relaxed),"wall_seconds":start.elapsed().as_secs_f64(),"boundary":"local SSE emit timestamp -> latest source marker observed after production drain_updates (32 updates/4ms, including retention); samples and skipped markers are post-pump view observations, not dequeued update counts; injected Paste -> matching TestBackend cells; DB worker queue+read RTT; 1ms harness polling and 30fps TestBackend, no terminal pixels or physical input"})
    );
    send.try_send(UiAction::Quit).unwrap();
    crate::test_watchdog::watchdog(
        "src/ui/performance_stream.rs: stream_input_layout_and_db_fairness",
        async {
            while !bridge.is_finished() {
                ui.drain_updates(&incoming);
                tokio::task::yield_now().await;
            }
        },
    )
    .await
    .unwrap();
    bridge.await.unwrap().unwrap();
}
