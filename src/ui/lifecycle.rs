//! One UI-owned shutdown display lifetime; core shutdown remains in Control.
use super::UiState;
type Progress = (u64, String, bool);
#[derive(Default)]
pub(super) enum Lifecycle {
    #[default]
    Running,
    Quitting {
        progress: Option<Progress>,
    },
    Stopped {
        forced: bool,
    },
}
impl Lifecycle {
    pub(super) fn begin(&mut self) {
        if matches!(self, Self::Running) {
            *self = Self::Quitting { progress: None };
        }
    }
    pub(super) fn accept_progress(&mut self, progress: Progress) {
        if !self.is_stopped() {
            *self = Self::Quitting {
                progress: Some(progress),
            };
        }
    }
    pub(super) fn reject_unaccepted(&mut self) {
        if matches!(self, Self::Quitting { progress: None }) {
            *self = Self::Running;
        }
    }
    pub(super) fn stop(&mut self, forced: bool) {
        *self = Self::Stopped {
            forced: forced || self.is_forced(),
        };
    }
    pub(super) fn progress(&self) -> Option<&Progress> {
        match self {
            Self::Quitting { progress } => progress.as_ref(),
            _ => None,
        }
    }
    pub(super) fn blocks_input(&self) -> bool {
        !matches!(self, Self::Running)
    }
    fn is_quitting(&self) -> bool {
        matches!(self, Self::Quitting { .. })
    }
    fn is_stopped(&self) -> bool {
        matches!(self, Self::Stopped { .. })
    }
    fn is_forced(&self) -> bool {
        matches!(self, Self::Stopped { forced: true })
    }
}
impl UiState {
    pub fn is_quitting(&self) -> bool {
        self.lifecycle.is_quitting()
    }
    pub fn is_stopped(&self) -> bool {
        self.lifecycle.is_stopped()
    }
    pub fn is_forced(&self) -> bool {
        self.lifecycle.is_forced()
    }
}
#[cfg(test)]
mod tests {
    use super::super::*;
    fn state() -> UiState {
        UiState::new(&Config::default(), PathBuf::from("/"))
    }
    fn quit(ui: &mut UiState) {
        assert!(
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::CONTROL
            )))
            .iter()
            .any(|action| matches!(action, UiAction::Quit))
        );
    }
    #[test]
    fn rejected_unaccepted_quit_recovers_but_accepted_progress_cannot_be_undone() {
        let mut ui = state();
        quit(&mut ui);
        assert!(ui.is_quitting() && !ui.is_stopped());
        ui.action_rejected(&UiAction::Quit);
        assert!(!ui.is_quitting() && !ui.is_stopped());
        ui.insert("retry remains editable", false);
        quit(&mut ui);
        ui.apply(UiUpdate::ShutdownProgress {
            elapsed_ms: 12,
            detail: "saving".into(),
            force_available: true,
        });
        ui.action_rejected(&UiAction::Quit);
        assert!(ui.is_quitting() && !ui.is_stopped());
        assert_eq!(ui.lifecycle.progress(), Some(&(12, "saving".into(), true)));
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::NONE,
        )));
        assert!(matches!(ui.confirmation, Some(Confirmation::ForceQuit)));
        assert!(
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE
            )))
            .iter()
            .any(|action| matches!(action, UiAction::ForceQuit))
        );
    }
    #[test]
    fn terminal_notification_keeps_force_fact_and_does_not_reopen_input_or_progress() {
        let mut ui = state();
        ui.insert("retained draft", false);
        let draft = ui.view().draft.clone();
        ui.apply(UiUpdate::ForceShutdown);
        ui.apply(UiUpdate::Shutdown);
        ui.apply(UiUpdate::ShutdownProgress {
            elapsed_ms: 99,
            detail: "late".into(),
            force_available: true,
        });
        ui.action_rejected(&UiAction::Quit);
        assert!(ui.is_stopped() && ui.is_forced() && !ui.is_quitting());
        assert!(ui.lifecycle.progress().is_none());
        assert!(ui.event(Event::Paste("must not edit".into())).is_empty());
        assert!(
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::CONTROL
            )))
            .is_empty()
        );
        assert_eq!(ui.view().draft, draft);
        let mut normal = state();
        normal.apply(UiUpdate::Shutdown);
        assert!(normal.is_stopped() && !normal.is_forced());
    }
}
