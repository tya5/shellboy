//! Current painted popup ownership and offset-only wheel dispatch.
use super::*;
impl UiState {
    pub(super) fn invalidate_popup_paint(&mut self) {
        self.popup_epoch = self.popup_epoch.wrapping_add(1);
        self.popup_paint = None;
    }
    pub(super) fn capture_popup_paint(&mut self) {
        let Some(owner) = self.top_overlay() else {
            return;
        };
        let bounds = self.popup_bounds(owner);
        self.popup_paint = bounds.map(|bounds| (owner, self.popup_epoch, bounds));
    }
    fn popup_bounds(&self, owner: overlay::Overlay) -> Option<popup::PaintBounds> {
        match owner {
            overlay::Overlay::Threads => self.thread_browser.paint_bounds(),
            overlay::Overlay::Models => self
                .models
                .as_ref()
                .and_then(models::ModelPicker::paint_bounds),
            overlay::Overlay::Operations => self
                .operations
                .as_ref()
                .and_then(operations::Operations::paint_bounds),
            overlay::Overlay::Branches => self
                .branches
                .as_ref()
                .and_then(branches::BranchPanel::paint_bounds),
            overlay::Overlay::Details => self
                .details
                .as_ref()
                .and_then(details::Details::paint_bounds),
            overlay::Overlay::Management => self.management.paint_bounds(),
            overlay::Overlay::Queue => self.queue_popup.paint_bounds(),
            overlay::Overlay::Help
            | overlay::Overlay::Confirmation
            | overlay::Overlay::Shutdown => self.aux_popup_paint.get(),
            overlay::Overlay::Selection => None,
        }
    }
    pub(super) fn popup_wheel(&mut self, mouse: event::MouseEvent) {
        if self.selection_mode {
            return;
        }
        let Some((owner, epoch, bounds)) = self.popup_paint else {
            return;
        };
        if self.top_overlay() != Some(owner)
            || self.popup_bounds(owner) != Some(bounds)
            || epoch != self.popup_epoch
            || bounds.body.width == 0
            || bounds.body.height == 0
            || !bounds
                .outer
                .contains(ratatui::layout::Position::new(mouse.column, mouse.row))
        {
            return;
        }
        let delta = if mouse.kind == MouseEventKind::ScrollUp {
            -3
        } else {
            3
        };
        let changed = match owner {
            overlay::Overlay::Help => {
                let old = self.help_scroll;
                self.help_scroll = popup::move_reader(old, delta, self.help_cache.bounds().maximum);
                old != self.help_scroll
            }
            overlay::Overlay::Threads => self.thread_browser.wheel(delta),
            overlay::Overlay::Models => self
                .models
                .as_mut()
                .is_some_and(|picker| picker.wheel(delta)),
            overlay::Overlay::Operations => self
                .operations
                .as_mut()
                .is_some_and(|operations| operations.wheel(delta)),
            overlay::Overlay::Branches => self
                .branches
                .as_mut()
                .is_some_and(|panel| panel.wheel(delta as isize)),
            overlay::Overlay::Details => {
                let view = &self.views[&self.active];
                self.details
                    .as_mut()
                    .is_some_and(|details| details.wheel(delta, view))
            }
            overlay::Overlay::Management => self.management.wheel(delta),
            overlay::Overlay::Queue => {
                let count = self.view().pending_messages.items.len();
                self.queue_popup.wheel(delta, count)
            }
            overlay::Overlay::Confirmation | overlay::Overlay::Shutdown
                if self.confirmation.is_some() =>
            {
                let old = self.confirmation_scroll;
                self.confirmation_scroll =
                    popup::move_reader(old, delta, self.confirmation_cache.bounds().maximum);
                old != self.confirmation_scroll
            }
            _ => false,
        };
        self.dirty |= changed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};
    fn paint(ui: &mut UiState, width: u16, height: u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, ui)).unwrap();
    }
    fn wheel(ui: &mut UiState, kind: MouseEventKind, x: u16, y: u16) {
        assert!(
            ui.event(Event::Mouse(MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE
            }))
            .is_empty(),
            "wheel must not submit domain actions"
        );
    }
    fn inside(ui: &UiState) -> (u16, u16) {
        let bounds = ui.popup_paint.unwrap().2;
        (bounds.body.x, bounds.body.y)
    }
    #[test]
    fn help_wheel_uses_painted_outer_only_edges_resize_and_f2_are_clean() {
        for (width, height) in [(80, 24), (140, 40), (24, 10)] {
            let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
            ui.insert("unsent draft", false);
            ui.show_help(help::Page::info(
                "Fixture",
                "reader",
                (0..200).map(|n| format!("saved row {n}\n")).collect(),
            ));
            paint(&mut ui, width, height);
            let before = ui.view().navigation.anchor.clone();
            let draft = ui.view().draft.text.clone();
            let (x, y) = inside(&ui);
            wheel(&mut ui, MouseEventKind::ScrollUp, x, y);
            assert!(!ui.dirty);
            wheel(&mut ui, MouseEventKind::ScrollDown, 0, 0);
            assert!(!ui.dirty);
            wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
            assert!(ui.dirty);
            assert_eq!(ui.help_scroll, 3);
            assert_eq!(ui.view().navigation.anchor, before);
            assert_eq!(ui.view().draft.text, draft);
            for _ in 0..200 {
                wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
            }
            ui.dirty = false;
            wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
            assert!(!ui.dirty, "bottom edge is a no-op");
            ui.event(Event::Resize(width + 1, height));
            assert!(ui.dirty, "resize still requests the normal repaint");
            assert!(
                ui.popup_paint.is_none(),
                "resize retires the old popup hit rectangle"
            );
            ui.dirty = false;
            wheel(&mut ui, MouseEventKind::ScrollUp, x, y);
            assert!(!ui.dirty, "old geometry is retired before paint");
            paint(&mut ui, width + 1, height);
            let (x, y) = inside(&ui);
            ui.event(Event::Key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)));
            ui.dirty = false;
            wheel(&mut ui, MouseEventKind::ScrollUp, x, y);
            assert!(!ui.dirty, "terminal selection owns mouse");
        }
    }
    #[test]
    fn replacement_reader_empty_list_confirmation_and_shutdown_keep_owner_and_no_action_rules() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.management.open_text(ManagementView {
            title: "old".into(),
            text: "old\n".repeat(100),
            scroll: 0,
        });
        paint(&mut ui, 100, 30);
        let (x, y) = inside(&ui);
        ui.management.open_text(ManagementView {
            title: "new".into(),
            text: "new\n".repeat(100),
            scroll: 0,
        });
        ui.dirty = false;
        wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
        assert!(
            !ui.dirty,
            "same-kind replacement must repaint before consuming geometry"
        );
        paint(&mut ui, 100, 30);
        let (x, y) = inside(&ui);
        wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
        assert!(ui.dirty);
        assert_eq!(ui.management.as_ref().unwrap().scroll, 3);
        ui.management.close();
        ui.thread_browser.open(String::new(), vec![], None);
        paint(&mut ui, 100, 30);
        let (x, y) = inside(&ui);
        wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
        assert!(!ui.dirty, "empty list is not scrollable");
        ui.thread_browser.close();
        ui.confirmation = Some(Confirmation::Index {
            command: Command::IndexStatus(IndexArgs::default()),
            attempt: 1,
            detail: "long detail\n".repeat(100),
        });
        ui.confirmation_cache.clear();
        paint(&mut ui, 100, 30);
        let (x, y) = inside(&ui);
        wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
        assert!(ui.dirty);
        assert_eq!(ui.confirmation_scroll, 3);
        assert!(ui.confirmation.is_some());
        ui.confirmation = None;
        ui.lifecycle.begin();
        paint(&mut ui, 100, 30);
        let (x, y) = inside(&ui);
        wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
        assert!(!ui.dirty, "shutdown status does not turn wheel into W/F");
    }
    #[test]
    fn queue_and_structured_management_wheel_move_selection_without_execution() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.queue_popup.visible = true;
        ui.queue_popup.archives = Some(
            (0..30)
                .map(|n| (format!("d{n}"), format!("draft {n}")))
                .collect(),
        );
        paint(&mut ui, 100, 30);
        let (x, y) = inside(&ui);
        wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
        assert_eq!(ui.queue_popup.selected, 3);
        assert!(ui.dirty);
        ui.queue_popup.visible = false;
        let fields = (0..30)
            .map(|n| PresentationField {
                key: format!("/{n}"),
                label: format!("field {n}"),
                value: format!("value {n}"),
                ..Default::default()
            })
            .collect();
        ui.management
            .open_structured("fixture".into(), 1, fields, false, false);
        paint(&mut ui, 100, 30);
        let (x, y) = inside(&ui);
        wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
        assert_eq!(ui.management.structured().unwrap().selected, 3);
        assert!(ui.dirty);
        for _ in 0..20 {
            wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
        }
        ui.dirty = false;
        wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
        assert!(!ui.dirty);
    }
    #[test]
    fn repeated_paints_wheels_and_confirmation_keys_reuse_wrapping_until_width_changes() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.show_help(help::Page::info(
            "Fixture",
            "source",
            "saved source row\n".repeat(4000),
        ));
        paint(&mut ui, 100, 30);
        assert_eq!(ui.help_cache.build_count(), 1);
        for _ in 0..10 {
            let (x, y) = inside(&ui);
            wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
            paint(&mut ui, 100, 30);
        }
        assert_eq!(ui.help_cache.build_count(), 1);
        ui.help_visible = false;
        ui.set_confirmation(Confirmation::Index {
            command: Command::IndexStatus(IndexArgs::default()),
            attempt: 1,
            detail: "cost facts\n".repeat(4000),
        });
        paint(&mut ui, 100, 30);
        assert_eq!(ui.confirmation_cache.build_count(), 1);
        for _ in 0..10 {
            let (x, y) = inside(&ui);
            wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::PageDown,
                KeyModifiers::NONE,
            )));
            paint(&mut ui, 100, 30);
        }
        assert_eq!(
            ui.confirmation_cache.build_count(),
            1,
            "height/page calculation also reuses wrapped rows"
        );
        paint(&mut ui, 120, 30);
        assert_eq!(ui.confirmation_cache.build_count(), 2);
    }
    #[test]
    fn confirmation_replacement_retires_the_prior_paint_and_prior_source_cache() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.set_confirmation(Confirmation::Index {
            command: Command::IndexStatus(IndexArgs::default()),
            attempt: 1,
            detail: "old source\n".repeat(100),
        });
        paint(&mut ui, 100, 30);
        let (x, y) = inside(&ui);
        ui.set_confirmation(Confirmation::ForceQuit);
        ui.dirty = false;
        wheel(&mut ui, MouseEventKind::ScrollDown, x, y);
        assert!(
            !ui.dirty,
            "an asynchronous replacement cannot consume the previous hit rectangle"
        );
        assert_eq!(ui.confirmation_scroll, 0);
        assert_eq!(ui.confirmation_cache.retained_bytes(), 0);
        paint(&mut ui, 100, 30);
        assert!(ui.confirmation.is_some());
    }
}
