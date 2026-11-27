use crate::{
    frontend::{control_tests, run},
    protocol::DEFAULT_LIST_LIMIT,
    ui::{UiAction, UiState, UiUpdate, actions},
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(crate) enum Observed {
    Baseline,
    /// A stale shown result found every command slot taken.
    RefreshDeferred,
    Page {
        generation: u64,
        stop: CancellationToken,
    },
}

tokio::task_local! {
    static OBSERVE: mpsc::Sender<Observed>;
}

pub(crate) async fn observe_run<T>(
    sender: mpsc::Sender<Observed>,
    run: impl Future<Output = T>,
) -> T {
    OBSERVE.scope(sender, run).await
}

pub(in crate::frontend) fn observe(event: Observed) {
    let _ = OBSERVE.try_with(|sender| sender.try_send(event).unwrap());
}

#[tokio::test]
async fn native_gap_retires_the_old_browser_page_and_reloads_one_bounded_page() {
    let (root, owner, control, server) = control_tests::fixture().await;
    let data = owner.db.clone();
    let mut ui = UiState::new(&control.store.config, root.path().into());
    let (send, receive) = actions::channel(32);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let (observe_send, mut observed) = mpsc::channel(4);
    let bridge = tokio::spawn(OBSERVE.scope(
        observe_send,
        run(
            control.clone(),
            receive,
            updates,
            root.path().into(),
            owner,
            server,
        ),
    ));
    crate::test_watchdog::watchdog("native saved event gap through TUI", async {
        assert!(matches!(observed.recv().await, Some(Observed::Baseline)));
        ui.insert("/threads", false);
        for action in ui.event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))) {
            send.try_send(action).unwrap();
        }
        assert!(ui.threads_visible());
        let Some(Observed::Page { generation, stop }) = observed.recv().await else {
            panic!("initial list page was not admitted");
        };
        loop {
            while let Ok(update) = incoming.try_recv() {
                ui.apply(update);
            }
            for action in ui.take_deferred_actions() {
                send.try_send(action).unwrap();
            }
            if !ui.threads().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(!stop.is_cancelled());
        assert_eq!(ui.list_generation(), generation);

        data.call(|db| {
            for i in 0..=DEFAULT_LIST_LIMIT {
                db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES (?1,'gap fixture','/','','p','m','2026-01-01T00:00:00Z')", [format!("reload-{i:03}")])?;
            }
            Ok(())
        }).await.unwrap();
        // On this single-threaded runtime the owner cannot consume the wait
        // between records in this burst. The saved-event cursor must expire.
        control.expire_saved_event_cursor_for_test();
        let mut requested = 0;
        let mut resyncs = 0;
        let mut pages = 0;
        loop {
            while let Ok(update) = incoming.try_recv() {
                match &update {
                    UiUpdate::BrowserResync { generation: retired, reason_en, .. } => {
                        assert_eq!(*retired, generation);
                        assert!(reason_en.as_deref().unwrap().contains("missed"));
                        assert!(stop.is_cancelled(), "old ticket still live at BrowserResync delivery");
                        resyncs += 1;
                    }
                    UiUpdate::Threads { generation: next, requested_cursor, threads, next_cursor, .. } => {
                        assert_ne!(*next, generation);
                        assert!(requested_cursor.is_none());
                        assert_eq!(threads.len(), DEFAULT_LIST_LIMIT as usize);
                        assert!(next_cursor.is_some());
                        pages += 1;
                    }
                    _ => {}
                }
                ui.apply(update);
            }
            for action in ui.take_deferred_actions() {
                if let UiAction::LoadThreads { generation: next, cursor, .. } = &action {
                    assert_ne!(*next, generation);
                    assert!(cursor.is_none());
                    requested += 1;
                }
                send.try_send(action).unwrap();
            }
            if pages > 0 { break; }
            tokio::task::yield_now().await;
        }
        assert_eq!((resyncs, requested, pages), (1, 1, 1));
        assert!(ui.threads().iter().any(|row| row.title == "gap fixture"));
        assert!(ui.threads_next_cursor().is_some());
        let Some(Observed::Page { generation: next, .. }) = observed.try_recv().ok() else {
            panic!("reload ticket was not admitted");
        };
        assert_eq!(next, ui.list_generation());
        assert!(observed.try_recv().is_err());
    }).await.unwrap();
    send.try_send(UiAction::Quit).unwrap();
    control_tests::finish_bridge(bridge, &incoming).await;
}

#[tokio::test]
async fn shown_config_result_is_read_again_when_a_saved_credential_change_wakes_the_wait() {
    use crate::protocol::{
        Command, ConfigGetArgs, CredentialsClearArgs, CredentialsSetArgs, Request,
    };
    let (root, owner, control, server) = control_tests::fixture().await;
    let (send, receive) = actions::channel(32);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let (observe_send, mut observed) = mpsc::channel(4);
    let bridge = tokio::spawn(OBSERVE.scope(
        observe_send,
        crate::frontend::run_with_clipboard_slots(
            control.clone(),
            receive,
            updates,
            root.path().into(),
            owner,
            server,
            std::sync::Arc::new(crate::frontend::ClipboardAdapter::new().unwrap()),
            slots.clone(),
        ),
    ));
    crate::test_watchdog::watchdog("F8 refresh on a saved change", async {
        assert!(matches!(observed.recv().await, Some(Observed::Baseline)));
        send.try_send(UiAction::Manage(Command::ConfigGet(ConfigGetArgs {})))
            .unwrap();
        let mut shown = 0;
        let mut changed = false;
        loop {
            while let Ok(update) = incoming.try_recv() {
                if matches!(update, UiUpdate::StructuredManagement { .. }) {
                    shown += 1;
                }
            }
            if shown == 1 && !changed {
                changed = true;
                let response = control
                    .request(Request::new(
                        format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
                        Command::CredentialsSet(CredentialsSetArgs {
                            connection: "fixture".into(),
                            value: "sk-fixture-key".into(),
                        }),
                    ))
                    .await;
                assert!(response.ok);
            }
            if shown == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        // With the only command slot taken, the next change is kept as one
        // pending refresh and read when the slot frees.
        let held = slots.clone().acquire_owned().await.unwrap();
        let response = control
            .request(Request::new(
                format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
                Command::CredentialsClear(CredentialsClearArgs {
                    connection: "fixture".into(),
                }),
            ))
            .await;
        assert!(response.ok);
        assert!(matches!(
            observed.recv().await,
            Some(Observed::RefreshDeferred)
        ));
        drop(held);
        // Any other command worker finishing frees the slot for the refresh.
        send.try_send(UiAction::CancelMessages {
            message_ids: vec!["unknown".into()],
            target: crate::ui::DraftRecoveryTarget {
                draft_id: "draft".into(),
                revision: 0,
                generation: 0,
                thread_id: None,
            },
        })
        .unwrap();
        loop {
            while let Ok(update) = incoming.try_recv() {
                if matches!(update, UiUpdate::StructuredManagement { .. }) {
                    shown += 1;
                }
            }
            if shown == 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    send.try_send(UiAction::Quit).unwrap();
    control_tests::finish_bridge(bridge, &incoming).await;
}

#[tokio::test]
async fn native_preview_is_scoped_answer_only_and_instance_fenced() {
    let (_root, owner, control, server) = control_tests::fixture().await;
    let data_dir = &owner.loaded.data_dir;
    let instance = &control.store.instance_id;
    let selected = control.fixture_live_text("thread");
    let hidden = control.fixture_live_text("independent");
    assert!(hidden.record(0, "hidden answer", false));
    assert!(selected.record(0, "synthetic reasoning", true));
    assert!(selected.record(19, "visible answer", false));
    selected.received(std::time::Instant::now());
    let page = super::wait_scoped(data_dir, instance, None, false, Some("thread".into()))
        .await
        .unwrap();
    let attempts = page.live_text.unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].thread_id, "thread");
    assert_eq!(attempts[0].attempt_id, "fixture-attempt");
    assert!(attempts[0].receive_age_ms.is_some());
    assert_eq!(attempts[0].deltas.len(), 1);
    assert_eq!(attempts[0].deltas[0].text, "visible answer");
    let error = super::wait_scoped(data_dir, "wrong-owner", None, false, Some("thread".into()))
        .await
        .unwrap_err();
    assert_eq!(error.code, crate::protocol::ErrorCode::ProtocolMismatch);
    drop(selected);
    drop(hidden);
    server.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

// Prepared fixture rows satisfy the same indexed admission read as a real frame.
pub(in crate::frontend) async fn install_preview_attempt(control: &crate::control::Control) {
    control.store.database.call(|db| {
        db.execute("UPDATE turn SET state='running' WHERE id='target-run'", [])?;
        db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('conversation','thread','fixture-attempt','target-run',0,0,'fixture','fixture-model',0,'inflight','{}','now')", [])?;
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn native_preview_resync_without_replacement_retires_displayed_suffix() {
    preview_gap(false).await;
}

#[tokio::test]
async fn native_empty_answer_gap_keeps_the_next_live_suffix_marked_incomplete() {
    preview_gap(true).await;
}

async fn preview_gap(still_live: bool) {
    let (root, owner, control, server) = control_tests::fixture().await;
    install_preview_attempt(&control).await;
    let live = control.fixture_live_text("thread");
    let mut ui = UiState::new(&control.store.config, root.path().into());
    let (send, receive) = actions::channel(32);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(run(
        control.clone(),
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    for action in ui.select_thread("thread".into()) {
        send.try_send(action).unwrap();
    }
    crate::test_watchdog::watchdog("native preview resync without replacement", async {
        // This attempt has an empty selected input. Adopt that saved tail
        // before emitting the single provisional frame.
        loop {
            if let Ok(update) = incoming.try_recv() {
                let tail = matches!(&update, UiUpdate::History {
                    thread_id, generation, append: false, total_nodes: 0, ..
                } if thread_id == "thread" && *generation == ui.view().generation);
                ui.apply(update);
                if tail && ui.view().turn_id.as_deref() == Some("target-run") {
                    break;
                }
            } else {
                tokio::task::yield_now().await;
            }
        }
        assert!(live.record(0, "provisional suffix", false));
        while !ui
            .view()
            .nodes()
            .iter()
            .any(|node| node.text == "provisional suffix")
        {
            while let Ok(update) = incoming.try_recv() {
                ui.apply(update);
            }
            tokio::task::yield_now().await;
        }
        let live = if still_live {
            // A single oversized reasoning delta evicts the earlier answer.
            // The owner omits all retained reasoning from the native page.
            let reasoning = "r".repeat(128 * 1024);
            assert!(live.record("provisional suffix".len() as u64, &reasoning, true));
            Some(live)
        } else {
            drop(live);
            control.expire_saved_event_cursor_for_test();
            None
        };
        loop {
            if let Ok(update) = incoming.try_recv() {
                let gap = matches!(&update, UiUpdate::PreviewGap { thread_id, generation }
                    if thread_id == "thread" && *generation == ui.view().generation);
                ui.apply(update);
                if gap {
                    assert!(
                        !ui.view()
                            .nodes()
                            .iter()
                            .any(|node| node.node_id == "fixture-attempt")
                    );
                    break;
                }
            } else {
                tokio::task::yield_now().await;
            }
        }
        if let Some(live) = live {
            assert!(live.record(
                ("provisional suffix".len() + 128 * 1024) as u64,
                "later answer",
                false
            ));
            loop {
                if let Ok(update) = incoming.try_recv() {
                    ui.apply(update);
                    if let Some(node) = ui
                        .view()
                        .nodes()
                        .iter()
                        .find(|node| node.text == "later answer")
                    {
                        assert_eq!(node.node_id, "fixture-attempt");
                        assert!(
                            node.presentation.body_extent_unknown,
                            "missing answer prefix must stay unknown"
                        );
                        break;
                    }
                } else {
                    tokio::task::yield_now().await;
                }
            }
            control.fixture_end_live_text("thread");
            loop {
                if let Ok(update) = incoming.try_recv() {
                    let ended = matches!(&update, UiUpdate::PreviewEnded { attempts, .. }
                        if attempts.iter().any(|attempt| attempt == "fixture-attempt"));
                    ui.apply(update);
                    if ended {
                        assert!(
                            !ui.view()
                                .nodes()
                                .iter()
                                .any(|node| node.node_id == "fixture-attempt")
                        );
                        break;
                    }
                } else {
                    tokio::task::yield_now().await;
                }
            }
        }
    })
    .await
    .unwrap();
    send.try_send(UiAction::Quit).unwrap();
    control_tests::finish_bridge(bridge, &incoming).await;
}
