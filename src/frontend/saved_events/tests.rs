use crate::{
    frontend::{control_tests, run},
    protocol::DEFAULT_LIST_LIMIT,
    ui::{UiAction, UiState, UiUpdate, actions},
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(in crate::frontend) enum Observed {
    Baseline,
    Page {
        generation: u64,
        stop: CancellationToken,
    },
}

tokio::task_local! {
    static OBSERVE: mpsc::Sender<Observed>;
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
