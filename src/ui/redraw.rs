//! Explicit repaint requests share the ordinary UI frame budget.
use super::*;
use crate::terminal_links::{LinkSnapshot, SharedLinkSnapshot};
use ratatui::backend::Backend;

pub(super) fn binding_available(config: &Config) -> bool {
    let key = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
    !key_matches(&config.ui.send_key, key)
        && !config
            .ui
            .newline_keys
            .iter()
            .any(|binding| key_matches(binding, key))
}

impl UiState {
    pub(super) fn request_full_redraw(&mut self, key: KeyEvent) -> bool {
        if !key_matches("Ctrl+L", key) || !binding_available(&self.config) {
            return false;
        }
        self.full_redraw_requested = true;
        self.dirty = true;
        true
    }
    pub(super) fn draw_due(
        &self,
        mode_changed: bool,
        elapsed: Duration,
        frame_time: Duration,
    ) -> bool {
        self.dirty
            && (!self.selection_mode || mode_changed || self.full_redraw_requested)
            && (mode_changed || elapsed >= frame_time)
    }
}

pub(super) fn draw_terminal<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &mut UiState,
    links: &SharedLinkSnapshot,
) -> Result<(), B::Error> {
    if state.full_redraw_requested {
        // Ratatui's clear() queries the physical cursor and can wait for a DSR
        // reply. Its fullscreen resize path performs the same screen/buffer
        // invalidation without that query. Keep the exact existing viewport;
        // the next draw restores the composer cursor and OSC 8 metadata.
        let area = terminal.get_frame().area();
        terminal.resize(area)?;
    }
    terminal.draw(|frame| {
        draw(frame, state);
        *links.borrow_mut() = LinkSnapshot {
            buffer: frame.buffer_mut().clone(),
            regions: state.visible_links.clone(),
        };
    })?;
    state.full_redraw_requested = false;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, buffer::Cell};
    use std::{cell::RefCell, rc::Rc};

    fn repaint() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL))
    }
    #[test]
    fn redraw_restores_foreign_cells_without_changing_any_input_owner() {
        for owner in [
            "composer",
            "help",
            "threads",
            "details",
            "operations",
            "models",
            "management",
            "confirmation",
            "selection",
            "shutdown",
        ] {
            let mut config = Config::default();
            config.connections.insert(
                "fixture".into(),
                crate::config::Connection {
                    base_url: "http://127.0.0.1:1/v1".into(),
                    ..Default::default()
                },
            );
            config.default_connection = Some("fixture".into());
            let mut ui = UiState::new(&config, std::env::temp_dir());
            ui.insert("keep this draft\n日本語", false);
            ui.view_mut().anchor = ViewAnchor {
                node_id: Some("retained".into()),
                char_offset: 37,
            };
            ui.view_mut().follow = false;
            ui.input_notice = Some("preserve existing notice");
            match owner {
                "help" => {
                    ui.slash("/help");
                }
                "threads" => {
                    ui.slash("/threads");
                }
                "details" => {
                    ui.event(Event::Key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
                }
                "operations" => {
                    ui.event(Event::Key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE)));
                }
                "models" => {
                    ui.slash("/models");
                }
                "management" => ui.apply(UiUpdate::StructuredManagement {
                    work_status: false,
                    request_id: 1,
                    title: "fixture".into(),
                    omitted: false,
                    fields: vec![],
                }),
                "confirmation" => ui.confirmation = Some(Confirmation::ForceQuit),
                "selection" => ui.selection_mode = true,
                "shutdown" => ui.lifecycle.begin(),
                _ => {}
            }
            let draft = ui.view().draft.clone();
            let anchor = ui.view().anchor.clone();
            let overlay = ui.top_overlay();
            let selected = ui.thread_selection();
            let fixed = ui.selection_mode;
            let notice = ui.input_notice;
            let snapshot = Rc::new(RefCell::new(LinkSnapshot::default()));
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            draw_terminal(&mut terminal, &mut ui, &snapshot).unwrap();
            let clean = terminal.backend().buffer().clone();
            // Change the real backend without updating Ratatui's prior frame.
            let foreign = Cell::new("#");
            terminal
                .backend_mut()
                .draw(std::iter::once((79, 10, &foreign)))
                .unwrap();
            assert_ne!(terminal.backend().buffer(), &clean, "{owner}");
            draw_terminal(&mut terminal, &mut ui, &snapshot).unwrap();
            assert_ne!(
                terminal.backend().buffer(),
                &clean,
                "ordinary diff cannot repair {owner}"
            );
            assert!(
                ui.event(repaint()).is_empty(),
                "no model or DB action for {owner}"
            );
            draw_terminal(&mut terminal, &mut ui, &snapshot).unwrap();
            assert_eq!(terminal.backend().buffer(), &clean, "{owner}");
            assert_eq!(ui.view().draft, draft, "{owner}");
            assert_eq!(ui.view().anchor, anchor, "{owner}");
            assert_eq!(ui.top_overlay(), overlay, "{owner}");
            assert_eq!(ui.thread_selection(), selected, "{owner}");
            assert_eq!(ui.selection_mode, fixed, "{owner}");
            assert_eq!(ui.input_notice, notice, "{owner}");
            assert!(!ui.view().follow, "{owner}");
        }
    }
    #[test]
    fn configured_send_and_newline_win_and_help_explains_the_conflict() {
        for send in [true, false] {
            let mut config = Config::default();
            if send {
                config.ui.send_key = "cTrL+l".into();
            } else {
                config.ui.newline_keys = vec!["Ctrl+L".into()];
            }
            let help = help::Page::guide(&config)
                .lines(160)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(help.contains("画面再描画は無効") && help.contains("設定と重複"));
            let mut ui = UiState::new(&config, std::env::temp_dir());
            ui.insert("preserved", false);
            let actions = ui.event(repaint());
            assert!(!ui.full_redraw_requested);
            if send {
                assert!(
                    actions
                        .iter()
                        .any(|a| matches!(a, UiAction::Send { text, .. } if text=="preserved"))
                );
            } else {
                assert!(actions.is_empty());
                assert_eq!(ui.view().draft.text, "preserved\n");
            }
        }
    }
    #[test]
    fn repeated_redraw_is_coalesced_under_the_frame_budget_even_while_selection_is_fixed() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.selection_mode = true;
        let frame = Duration::from_millis(100);
        assert!(!ui.draw_due(false, frame, frame));
        for _ in 0..100 {
            assert!(ui.event(repaint()).is_empty());
        }
        assert!(!ui.draw_due(false, frame - Duration::from_nanos(1), frame));
        assert!(ui.draw_due(false, frame, frame));
        let snapshot = Rc::new(RefCell::new(LinkSnapshot::default()));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        draw_terminal(&mut terminal, &mut ui, &snapshot).unwrap();
        assert!(ui.selection_mode);
        assert!(!ui.full_redraw_requested);
        assert!(!ui.draw_due(false, frame, frame));
        let mut release = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
        release.kind = KeyEventKind::Release;
        assert!(ui.event(Event::Key(release)).is_empty());
        assert!(!ui.full_redraw_requested);
    }
}
