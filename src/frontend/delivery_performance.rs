//! Real frontend bridge under bounded synthetic UI/event pressure, no model API.
use super::*;
use std::sync::atomic::Ordering;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "synthetic ten-thread event flood with 250ms stopped UI; run serially with --nocapture"]
async fn performance_slow_ui_bridge_action_and_event_progress() {
    let (root, owner, control, server) = control_tests::fixture().await;
    saved_events::tests::install_preview_attempt(&control).await;
    let (sender, receiver) = crate::ui::actions::channel(128);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let pressure = updates.clone();
    let bridge = tokio::spawn(run(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    sender
        .try_send(UiAction::SelectThread {
            thread_id: "thread".into(),
            generation: 0,
            view_revision: 0,
            known_view: None,
        })
        .unwrap();
    for _ in 0..30 {
        while incoming.try_recv().is_ok() {}
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    stream_metrics::reset();
    let mut actions = vec![];
    let mut preview_frames = 0;
    for trial in 0..7 {
        // Model/presentation workers can fill this same finite output channel.
        // Injecting pre-existing UI work makes the pressure deterministic without
        // changing product scheduling, channel sizes or introducing API calls.
        while pressure
            .try_send(UiUpdate::Status {
                thread_id: None,
                text: "synthetic queued UI work".into(),
            })
            .is_ok()
        {}
        tokio::time::sleep(Duration::from_millis(10)).await;
        let before = stream_metrics::FRONTEND_ACTIONS.load(Ordering::Relaxed);
        let start = std::time::Instant::now();
        sender
            .try_send(UiAction::RenderWidth { width: 80 + trial })
            .unwrap();
        let producer_control = control.clone();
        let producer = tokio::spawn(async move {
            let threads = (0..10)
                .map(|n| {
                    if n == 0 {
                        "thread".into()
                    } else {
                        format!("synthetic-{n}")
                    }
                })
                .collect::<Vec<_>>();
            let attempts = threads
                .iter()
                .map(|thread| producer_control.fixture_live_text(thread))
                .collect::<Vec<_>>();
            for sequence in 0..100 {
                for attempt in &attempts {
                    assert!(attempt.record(sequence * 5, "delta", false));
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });
        let mut latency = None;
        let stopped_until = tokio::time::Instant::now() + Duration::from_millis(250);
        while tokio::time::Instant::now() < stopped_until {
            if latency.is_none()
                && stream_metrics::FRONTEND_ACTIONS.load(Ordering::Relaxed) > before
            {
                latency = Some(start.elapsed());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let deadline = crate::test_watchdog::Deadline::new("UI drain resumes bridge");
        while latency.is_none() || !producer.is_finished() {
            while let Ok(update) = incoming.try_recv() {
                if let UiUpdate::Stream {
                    thread_id,
                    text,
                    ack: Some(ack),
                    ..
                } = update
                {
                    assert_eq!(thread_id, "thread");
                    assert!(text.contains("delta"));
                    ack.notify_one();
                    preview_frames += 1;
                }
            }
            if latency.is_none()
                && stream_metrics::FRONTEND_ACTIONS.load(Ordering::Relaxed) > before
            {
                latency = Some(start.elapsed());
            }
            assert!(!deadline.expired(), "bridge did not resume after UI drain");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        producer.await.unwrap();
        actions.push(latency.unwrap().as_secs_f64() * 1000.0);
        for _ in 0..10 {
            while let Ok(update) = incoming.try_recv() {
                if let UiUpdate::Stream {
                    thread_id,
                    text,
                    ack: Some(ack),
                    ..
                } = update
                {
                    assert_eq!(thread_id, "thread");
                    assert!(text.contains("delta"));
                    ack.notify_one();
                    preview_frames += 1;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    assert!(
        preview_frames > 0,
        "pressure did not exercise native preview delivery"
    );
    actions.sort_by(f64::total_cmp);
    println!(
        "PERF {}",
        serde_json::json!({"name":"slow_ui_bridge","samples":actions.len(),"threads":10,"deltas_per_trial":1000,"ui_stopped_ms":250,
        "action_p50_ms":actions[3],"action_p95_ms":actions[6],"reloads":stream_metrics::FRONTEND_RELOADS.load(Ordering::Relaxed)})
    );
    sender.try_send(UiAction::Quit).unwrap();
    control_tests::finish_bridge(bridge, &incoming).await;
}
