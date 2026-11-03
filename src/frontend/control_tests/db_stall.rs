//! A test-only foreground gate, released explicitly (or on panic), not by time.
use super::*;
use crate::{
    storage::DbWorker,
    ui::{ReceiptStatus, UiState, UiUpdate},
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

struct ForegroundGate {
    release: Option<mpsc::Sender<()>>,
    job: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for ForegroundGate {
    fn drop(&mut self) {
        self.release.take();
    }
}
impl ForegroundGate {
    async fn start(db: DbWorker) -> Self {
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, held) = mpsc::channel();
        let job = tokio::spawn(async move {
            db.call(move |_| {
                let _ = started.send(());
                let _ = held.recv();
                Ok(())
            })
            .await
            .unwrap();
        });
        ready.await.unwrap();
        Self {
            release: Some(release),
            job: Some(job),
        }
    }
    async fn finish(mut self) {
        self.release.take();
        self.job.take().unwrap().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_db_stall_keeps_submission_input_and_status_delivery_responsive() {
    for language in ["en", "ja"] {
        let (root, owner, control, server) = fixture_with_language(language).await;
        let gate = ForegroundGate::start(owner.db.clone()).await;
        let (sender, receiver) = actions::channel(16);
        let (updates, incoming) = mpsc::sync_channel(128);
        let mut ui = UiState::new(&control.store.config, root.path().into());
        ui.apply(UiUpdate::Initialize {
            cwd: root.path().into(),
        });
        let _ = ui.select_thread("thread".into());
        ui.insert("frozen request", false);
        let revision = ui.view().draft.revision;
        let action = ui
            .event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .remove(0);
        assert!(matches!(action, UiAction::Send { .. }));
        assert!(ui.view().draft.text.is_empty());
        let bridge = tokio::spawn(super::super::run_with_clipboard_slots(
            control.clone(),
            receiver,
            updates,
            root.path().into(),
            owner,
            server,
            std::sync::Arc::new(super::UnusedClipboard),
            std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
        ));
        sender.try_send(action).unwrap();
        // Acknowledgement and the activity snapshot both bypass the occupied DB.
        crate::test_watchdog::watchdog("foreground DB stall updates", async {
            let mut operation = false;
            let mut activity = false;
            while !operation || !activity {
                while let Ok(update) = incoming.try_recv() {
                    operation |= matches!(&update, UiUpdate::SendOperation { .. });
                    activity |=
                        matches!(&update, UiUpdate::DbWorkerActivity(a) if a.active.is_some());
                    assert!(!matches!(&update, UiUpdate::SavedMessage { .. }));
                    ui.apply(update);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!gate.job.as_ref().unwrap().is_finished());
        ui.insert("next draft", false);
        let next = ui.view().draft.clone();
        control.emit_test_event(crate::control::EngineEvent::Error {
            thread_id: "thread".into(),
            error: crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::InvalidState,
                "status during stalled DB",
            ),
        });
        crate::test_watchdog::watchdog("foreground DB stall updates", async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    let observed = matches!(&update, UiUpdate::Status { text, .. } if text.contains("status during stalled DB"));
                    ui.apply(update);
                    if observed { return; }
                }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let display_now = Instant::now();
        ui.animation_tick(display_now + Duration::from_secs(227));
        assert_eq!(ui.view().draft, next);
        assert_eq!(ui.view().pending[&revision].snapshot.text, "frozen request");
        assert_eq!(
            ui.view().pending[&revision].status,
            ReceiptStatus::Accepting
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut ui))
            .unwrap();
        let screen = screen_text(&terminal);
        assert!(screen.contains("next draft"), "{screen}");
        assert!(
            screen.contains(if language == "en" {
                "Accepting input"
            } else {
                "送信受付中"
            }),
            "{screen}"
        );
        assert!(screen.contains("status during stalled DB"), "{screen}");
        assert!(!gate.job.as_ref().unwrap().is_finished());
        gate.finish().await;
        crate::test_watchdog::watchdog("foreground DB stall updates", async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    let saved = matches!(&update, UiUpdate::SavedMessage { .. });
                    ui.apply(update);
                    if saved {
                        return;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            ui.view().draft,
            next,
            "late saved projection preserves newer input"
        );
        assert!(!ui.view().pending.contains_key(&revision));
        sender.try_send(UiAction::Quit).unwrap();
        finish_bridge(bridge, &incoming).await;
    }
}

fn screen_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    use crate::ui::text_width::UnicodeWidthStr;
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        let mut x = 0;
        while x < buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            text.push_str(symbol);
            x += symbol.width().max(1) as u16;
        }
        text.push('\n');
    }
    text
}
