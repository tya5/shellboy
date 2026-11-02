//! UI input ownership, derived from existing overlay state without replacing stacking.
use super::i18n::local;
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Overlay {
    Shutdown,
    Confirmation,
    Selection,
    Models,
    Operations,
    Details,
    Management,
    Queue,
    Help,
    Threads,
}
impl UiState {
    fn top_popup(&self) -> Option<Overlay> {
        if self.lifecycle.blocks_input() {
            Some(Overlay::Shutdown)
        } else if self.confirmation.is_some() {
            Some(Overlay::Confirmation)
        } else if self.help_visible && self.help_above_popups {
            Some(Overlay::Help)
        } else if self.models.is_some() {
            Some(Overlay::Models)
        } else if self.operations.is_some() {
            Some(Overlay::Operations)
        } else if self.details.is_some() {
            Some(Overlay::Details)
        } else if self.queue_popup.visible {
            Some(Overlay::Queue)
        } else if self.management.is_some() {
            Some(Overlay::Management)
        } else if self.help_visible {
            Some(Overlay::Help)
        } else if self.thread_browser.visible() {
            Some(Overlay::Threads)
        } else {
            None
        }
    }
    pub(super) fn top_overlay(&self) -> Option<Overlay> {
        let popup = self.top_popup();
        if self.selection_mode && !matches!(popup, Some(Overlay::Shutdown | Overlay::Confirmation))
        {
            Some(Overlay::Selection)
        } else {
            popup
        }
    }
    pub(super) fn composer_active(&self) -> bool {
        self.top_overlay().is_none()
    }
    pub(super) fn history_mouse_active(&self) -> bool {
        self.composer_active() && !self.has_completions()
    }
    pub(super) fn history_links_active(&self) -> bool {
        self.history_mouse_active()
    }
    pub fn event(&mut self, event: Event) -> Vec<UiAction> {
        if matches!(&event, Event::Paste(_))
            || matches!(&event, Event::Key(key) if key.kind != KeyEventKind::Release)
        {
            self.invalidate_popup_paint();
        }
        // A redraw gesture has no owner-specific input or status side effects.
        if let Event::Key(key) = &event
            && key.kind != KeyEventKind::Release
            && self.request_full_redraw(*key)
        {
            return vec![];
        }
        if matches!(&event, Event::Paste(_))
            || matches!(&event, Event::Key(key) if key.kind != KeyEventKind::Release)
        {
            self.input_notice = None;
            self.active_view_mut().clear_operation_notice();
        }
        let status_before = self.view().status.clone();
        let closes_management = matches!(&event,Event::Key(key) if key.code==KeyCode::Esc)
            && self.top_popup().is_none();
        if closes_management {
            self.management.epoch = self.management.epoch.wrapping_add(1);
        }
        if !self.composer_active() {
            self.immediate_chord = false;
        }
        let input_owner = self.active.clone();
        let selection_before = self.selection_mode;
        let follow_before = self.view().navigation.follow();
        let actions = self.event_inner(event);
        if self.selection_mode != selection_before {
            self.selection_frozen_at = self
                .selection_mode
                .then(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        }
        if self.active == input_owner && self.view().navigation.follow() != follow_before {
            let view = self.view();
            self.trace_history(crate::diagnostics::UiHistoryTrace {
                reason: crate::diagnostics::UiHistoryReason::UserFollow,
                generation: view.generation,
                view_revision: view.navigation.view_revision,
                read_revision: view.delivery_read_revision,
                follow_before,
                follow_after: view.navigation.follow(),
                before_nodes: view.nodes().len() as u64,
                after_nodes: view.nodes().len() as u64,
                ..Default::default()
            });
        }
        if self.active != input_owner {
            self.immediate_chord = false;
        }
        if !self.composer_active() {
            self.immediate_chord = false;
        }
        if self.active == input_owner && self.view().status != status_before {
            self.active_view_mut()
                .promote_status_notice(history_interaction::NoticeKind::Info);
        }
        self.scope_management_actions(actions)
    }
    pub(super) fn scope_management_actions(&mut self, mut actions: Vec<UiAction>) -> Vec<UiAction> {
        fn needs_scope(action: &UiAction) -> bool {
            match action {
                UiAction::SubmittedCommand { action, .. } => needs_scope(action),
                UiAction::LoadReceived { .. }
                | UiAction::Manage(_)
                | UiAction::ClientOperation(_)
                | UiAction::ReloadAttachments { .. }
                | UiAction::IndexPreview { .. }
                | UiAction::FollowManagement { .. }
                | UiAction::LoadManagementDetail { .. } => true,
                UiAction::Command(command) => {
                    !matches!(command, Command::ThreadSet(_) | Command::RunRetry(_))
                }
                _ => false,
            }
        }
        fn scope(action: UiAction, context: &ManagementContext) -> UiAction {
            match action {
                UiAction::SubmittedCommand { submission, action } => UiAction::SubmittedCommand {
                    submission,
                    action: Box::new(scope(*action, context)),
                },
                action if needs_scope(&action) => UiAction::ManagementIntent {
                    context: context.clone(),
                    action: Box::new(action),
                },
                action => action,
            }
        }

        fn starts_scope(action: &UiAction) -> bool {
            match action {
                UiAction::SubmittedCommand { action, .. } => starts_scope(action),
                UiAction::LoadManagementDetail { .. } => false,
                action => needs_scope(action),
            }
        }
        if actions.iter().any(needs_scope) {
            if actions.iter().any(starts_scope) {
                self.management.epoch = self.management.epoch.wrapping_add(1);
            }
            let current = ManagementContext {
                epoch: self.management.epoch,
                owner_draft: self.view().draft.draft_id.clone(),
                owner_thread: self.view().thread_id.clone(),
            };
            let context = if actions.iter().any(starts_scope) {
                self.management.owner = Some(current.clone());
                current
            } else {
                self.management.owner.clone().unwrap_or(current)
            };
            actions = actions
                .into_iter()
                .map(|action| scope(action, &context))
                .collect();
        }
        for action in &mut actions {
            self.stamp_inspection_action(action);
        }
        actions
    }
    fn event_inner(&mut self, event: Event) -> Vec<UiAction> {
        match event {
            Event::Paste(text) => {
                self.dirty = true;
                match self.top_overlay() {
                    Some(Overlay::Models) => self.models.as_mut().unwrap().paste(&text),
                    Some(Overlay::Operations) => self.operations.as_mut().unwrap().paste(&text),
                    None => self.insert(&text, true),
                    Some(Overlay::Selection) => {
                        self.selection_mode = false;
                        self.input_notice = Some(local!(
                            self.config.ui.language,
                            "未反映: 選択モードを解除したので再貼付してください",
                            "Denied: Selection mode ended; paste again"
                        ));
                    }
                    Some(Overlay::Shutdown) => {
                        self.input_notice = Some(local!(
                            self.config.ui.language,
                            "未反映: 終了処理中は貼付けできません",
                            "Denied: Cannot paste during shutdown"
                        ))
                    }
                    Some(Overlay::Confirmation) => {
                        self.input_notice = Some(local!(
                            self.config.ui.language,
                            "未反映: 確認を閉じてから再貼付してください",
                            "Denied: Close the confirmation and paste again"
                        ))
                    }
                    _ => {
                        self.input_notice = Some(local!(
                            self.config.ui.language,
                            "未反映: Escで戻って再貼付してください",
                            "Denied: Press Esc, then paste again"
                        ))
                    }
                }
                vec![]
            }
            Event::Resize(width, height) => {
                let viewport = Rect::new(0, 0, width, height);
                if self.viewport != viewport {
                    self.thread_browser.invalidate_paint();
                    self.invalidate_popup_paint();
                }
                self.dirty |= self.viewport != viewport;
                self.viewport = viewport;
                vec![]
            }
            Event::Mouse(mouse)
                if self.top_overlay().is_some()
                    && matches!(
                        mouse.kind,
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    ) =>
            {
                self.popup_wheel(mouse);
                vec![]
            }
            Event::Mouse(mouse)
                if self.history_mouse_active()
                    && self.top_overlay().is_none()
                    && mouse.kind == MouseEventKind::Down(event::MouseButton::Left)
                    && self.live_work_click(mouse.column, mouse.row) =>
            {
                vec![]
            }
            Event::Mouse(mouse)
                if self.history_mouse_active()
                    && self.view().navigation.scroll.area.is_none_or(|area| {
                        area.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                    }) =>
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        self.dirty = true;
                        self.scroll(if mouse.kind == MouseEventKind::ScrollUp {
                            -3
                        } else {
                            3
                        })
                    }
                    MouseEventKind::Down(event::MouseButton::Left) => {
                        self.active_view_mut().live_work.active = false;
                        self.history_click(mouse.column, mouse.row)
                    }
                    _ => vec![],
                }
            }
            Event::Mouse(mouse)
                if self.top_overlay().is_none()
                    && mouse.kind == MouseEventKind::Down(event::MouseButton::Left) =>
            {
                self.active_view_mut().live_work.active = false;
                vec![]
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.dirty = true;
                self.key(key)
            }
            _ => vec![],
        }
    }
    fn submit_immediately(&mut self) -> Vec<UiAction> {
        self.send_now = true;
        let actions = self.submit();
        self.send_now = false;
        actions
    }
    pub(super) fn key(&mut self, key: KeyEvent) -> Vec<UiAction> {
        if let Some(actions) = self.global_key(key) {
            return actions;
        }
        if self.top_overlay() == Some(Overlay::Help)
            && matches!(key.code, KeyCode::F(6) | KeyCode::F(7) | KeyCode::F(8))
        {
            self.help_visible = false;
        }
        if let Some(
            owner @ (Overlay::Confirmation
            | Overlay::Selection
            | Overlay::Models
            | Overlay::Operations
            | Overlay::Details),
        ) = self.top_overlay()
        {
            return self.overlay_key(owner, key);
        }
        if self.composer_active()
            && key.code == KeyCode::Char('d')
            && key.modifiers == KeyModifiers::ALT
            && !key_matches(&self.config.ui.send_key, key)
            && !self
                .config
                .ui
                .newline_keys
                .iter()
                .any(|binding| key_matches(binding, key))
        {
            return self.open_descendant_watch();
        }
        if self.composer_active()
            && key.code == KeyCode::Char('w')
            && key.modifiers == KeyModifiers::ALT
            && super::live_work::section_key_available(&self.config)
        {
            self.toggle_live_work_section();
            return vec![];
        }
        if let Some(actions) = self.live_work_key(key) {
            return actions;
        }
        if key.code == KeyCode::F(6) {
            return self.open_history_details(false, false);
        }
        if key.code == KeyCode::F(7) {
            return self.open_history_details(true, false);
        }
        if key.code == KeyCode::F(8) {
            self.open_operations(None);
            return vec![];
        }
        if self.composer_active()
            && matches!(key.code, KeyCode::F(3) | KeyCode::F(4) | KeyCode::F(5))
        {
            if let Some(fold) = self.view().draft.folds.last().cloned() {
                let mode = if key.code == KeyCode::F(5) {
                    "remove"
                } else if key.code == KeyCode::F(4) {
                    "edit"
                } else {
                    "show"
                };
                return self.paste_command(&format!("{} {mode}", fold.id));
            }
            return vec![];
        }
        if let Some(owner) = self.top_overlay() {
            return self.overlay_key(owner, key);
        }
        if key.code == KeyCode::F(1) {
            return self.show_help(help::Page::guide(&self.config));
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('b') => {
                    return self
                        .view()
                        .thread_id
                        .clone()
                        .map(|thread_id| vec![UiAction::Background { thread_id }])
                        .unwrap_or_default();
                }
                KeyCode::Char('t') => {
                    return self.slash(
                        if self.thread_browser.mode() == watch::BrowserMode::Watch {
                            "/threads active"
                        } else {
                            "/threads"
                        },
                    );
                }
                KeyCode::Char('n') => return self.new_chat(),
                KeyCode::End => {
                    let view = self.active_view_mut();
                    view.items.active = false;
                    view.live_work.active = false;
                    view.follow_tail();
                    return view
                        .thread_id
                        .clone()
                        .map(|thread_id| {
                            vec![UiAction::LoadHistory {
                                thread_id,
                                generation: view.generation,
                                before_cursor: None,
                            }]
                        })
                        .unwrap_or_default();
                }
                _ => {}
            }
        }
        if let Some(actions) = self.history_focus_key(key) {
            return actions;
        }
        if self.immediate_chord {
            self.immediate_chord = false;
            if key_matches("Ctrl+S", key) {
                return self.submit_immediately();
            }
        }
        if key_matches("Ctrl+X", key) {
            self.immediate_chord = true;
            return vec![];
        }
        if key_matches("Ctrl+Enter", key) && !key_matches(&self.config.ui.send_key, key) {
            return self.submit_immediately();
        }
        if let Some(actions) = self.composer_key(key) {
            return actions;
        }
        if key.code == KeyCode::Esc
            && self
                .view()
                .activity
                .snapshot
                .as_ref()
                .is_some_and(|s| s.turn_in_progress)
            && let (Some(thread_id), Some(turn_id)) = (
                self.view().thread_id.clone(),
                self.view()
                    .activity
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.turn_id.clone()),
            )
        {
            return vec![UiAction::InterruptTurn { thread_id, turn_id }];
        }
        if key_matches(&self.config.ui.send_key, key) {
            return self.submit();
        }
        if self
            .config
            .ui
            .newline_keys
            .iter()
            .any(|binding| key_matches(binding, key))
        {
            self.insert("\n", false);
            return vec![];
        }
        if key_matches("Ctrl+U", key) {
            self.clear_current_draft();
            return vec![];
        }
        match key.code {
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&ch.to_string(), false)
            }
            KeyCode::Backspace => self.delete(true),
            KeyCode::Delete => self.delete(false),
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {
                self.active_view_mut().draft.move_cursor(key.code);
            }
            KeyCode::PageUp => {
                return self.scroll(
                    -(self
                        .view()
                        .navigation
                        .scroll
                        .height
                        .saturating_sub(1)
                        .max(1) as isize),
                );
            }
            KeyCode::PageDown => {
                return self.scroll(
                    self.view()
                        .navigation
                        .scroll
                        .height
                        .saturating_sub(1)
                        .max(1) as isize,
                );
            }
            KeyCode::Up | KeyCode::Down => self.active_view_mut().draft.move_cursor(key.code),
            _ => {}
        }
        vec![]
    }
    fn global_key(&mut self, key: KeyEvent) -> Option<Vec<UiAction>> {
        if self.is_stopped() {
            return Some(vec![]);
        }
        if self.is_quitting() {
            return Some(self.shutdown_key(key));
        }
        if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.lifecycle.begin();
            self.selection_mode = false;
            self.confirmation = None;
            let mut actions = self.persist_all();
            actions.push(UiAction::Quit);
            return Some(actions);
        }
        if key.code == KeyCode::F(1)
            && self.confirmation.is_none()
            && self.top_overlay() != Some(Overlay::Threads)
            && !(self.top_overlay() == Some(Overlay::Operations)
                && self
                    .operations
                    .as_ref()
                    .is_some_and(|operations| operations.owns_form_help()))
        {
            self.selection_mode = false;
            if self.top_overlay() == Some(Overlay::Help) {
                self.help_visible = false;
                return Some(vec![]);
            }
            let above = self.top_overlay().is_some();
            let actions = self.show_help(help::Page::guide(&self.config));
            self.help_above_popups = above;
            return Some(actions);
        }
        // Terminal selection shortcuts may consume Ctrl+C before it reaches us;
        // when delivered to the app it always retains its interrupt meaning.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(
                self.view()
                    .thread_id
                    .clone()
                    .map(|thread_id| vec![UiAction::Cancel { thread_id }])
                    .unwrap_or_default(),
            );
        }
        if self.confirmation.is_none()
            && (key.code == KeyCode::F(2) || (self.selection_mode && key.code == KeyCode::Esc))
        {
            self.selection_mode = !self.selection_mode;
            return Some(vec![]);
        }
        None
    }
    fn overlay_key(&mut self, owner: Overlay, key: KeyEvent) -> Vec<UiAction> {
        match owner {
            Overlay::Queue => self.queue_key(key),
            Overlay::Models => self.models_key(key),
            Overlay::Operations => {
                let outcome = self.operations.as_mut().unwrap().key(key);
                match outcome {
                    operations::Outcome::Close => {
                        self.operations = None;
                        vec![]
                    }
                    operations::Outcome::Execute(command) => {
                        self.operations = None;
                        self.execute_operation(command)
                    }
                    operations::Outcome::Client(operation) => {
                        self.operations = None;
                        self.execute_client_operation(operation)
                    }
                    operations::Outcome::None => vec![],
                }
            }
            Overlay::Details => self.detail_key(key),
            Overlay::Management => self.management_key(key),
            Overlay::Help => {
                let page = popup::body_area(
                    popup::viewport(self.viewport),
                    self.help_page.context,
                    help::footer(self.config.ui.language),
                    None,
                )
                .height
                .saturating_sub(1)
                .max(1);
                if let Some(scroll) = popup::reader_key(key.code, self.help_scroll, page) {
                    self.help_scroll = scroll;
                }
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::F(1)) {
                    self.help_visible = false;
                }
                vec![]
            }
            Overlay::Threads => self.thread_key(key),
            Overlay::Shutdown => self.shutdown_key(key),
            Overlay::Confirmation => self.confirmation_key(key),
            Overlay::Selection => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> UiState {
        let mut config = Config {
            default_connection: Some("fixture".into()),
            ..Config::default()
        };
        config.connections.insert(
            "fixture".into(),
            crate::config::Connection {
                base_url: "http://127.0.0.1:9/v1".into(),
                ..Default::default()
            },
        );
        UiState::new(&config, PathBuf::from("/"))
    }
    #[test]
    fn invalid_watch_confirmation_shows_localized_notice_without_action() {
        use super::super::text_width::UnicodeWidthStr;
        use ratatui::{Terminal, backend::TestBackend};
        for (language, expected) in [
            (
                crate::config::UiLanguage::Ja,
                "対象が変わったため確認を取り消しました。選び直してください",
            ),
            (
                crate::config::UiLanguage::En,
                "The target changed, so confirmation was cancelled. Select it again",
            ),
        ] {
            let mut ui = state();
            ui.config.ui.language = language;
            ui.confirmation = Some(Confirmation::Watch(watch::WatchActionTarget {
                browser_generation: u64::MAX,
                thread_id: "thread-fixture".into(),
                run_id: "run-fixture".into(),
                turn_id: None,
                turn_generation: None,
                phase: crate::activity::ActivityPhase::Running,
                execution_id: None,
                kind: watch::WatchActionKind::Stop,
            }));
            assert!(
                ui.confirmation_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
                    .is_empty()
            );
            assert!(ui.confirmation.is_none());
            let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
            terminal
                .draw(|frame| ui.thread_browser.draw(frame, frame.area(), None, true))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let mut text = String::new();
            for y in 0..30 {
                let mut x = 0;
                while x < 160 {
                    let symbol = buffer[(x, y)].symbol();
                    text.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                text.push('\n');
            }
            assert!(text.contains(expected), "{language:?}: {text}");
        }
    }

    #[test]
    fn changed_edit_confirmation_is_cancelled_in_both_languages() {
        for (language, expected) in [
            (
                crate::config::UiLanguage::Ja,
                "下書きが変更されたため確認を取り消しました。再送してください",
            ),
            (
                crate::config::UiLanguage::En,
                "The draft changed, so confirmation was cancelled. Send again",
            ),
        ] {
            let mut ui = state();
            ui.config.ui.language = language;
            ui.confirmation = Some(Confirmation::EditStop {
                draft_id: ui.view().draft.draft_id.clone(),
                revision: ui.view().draft.revision.wrapping_add(1),
                run_id: "run-fixture".into(),
            });
            assert!(
                ui.confirmation_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
                    .is_empty()
            );
            assert!(ui.confirmation.is_none());
            assert_eq!(ui.view().status, expected);
        }
    }

    #[test]
    fn ctrl_t_remembers_browser_mode_but_explicit_threads_and_new_ui_start_in_list() {
        fn mode(actions: &[UiAction]) -> watch::BrowserMode {
            match actions {
                [UiAction::LoadThreads { mode, .. }] => *mode,
                _ => panic!("one browser request expected"),
            }
        }
        fn close(ui: &mut UiState) {
            ui.event(key(KeyCode::Esc, KeyModifiers::NONE));
            assert!(!ui.threads_visible());
        }
        let mut ui = state();
        assert_eq!(
            mode(&ui.event(key(KeyCode::Char('t'), KeyModifiers::CONTROL))),
            watch::BrowserMode::List
        );
        close(&mut ui);
        assert_eq!(
            mode(&ui.slash("/threads active")),
            watch::BrowserMode::Watch
        );
        close(&mut ui);
        assert_eq!(
            mode(&ui.event(key(KeyCode::Char('t'), KeyModifiers::CONTROL))),
            watch::BrowserMode::Watch
        );
        assert_eq!(
            mode(&ui.event(key(KeyCode::Tab, KeyModifiers::NONE))),
            watch::BrowserMode::List
        );
        close(&mut ui);
        assert_eq!(
            mode(&ui.event(key(KeyCode::Char('t'), KeyModifiers::CONTROL))),
            watch::BrowserMode::List
        );
        close(&mut ui);
        ui.slash("/threads active");
        close(&mut ui);
        assert_eq!(mode(&ui.slash("/threads filter")), watch::BrowserMode::List);
        assert_eq!(ui.thread_filter(), "filter");
        close(&mut ui);
        ui.thread_browser
            .open_watch(String::new(), Some("old-root".into()), None);
        close(&mut ui);
        let actions = ui.event(key(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(mode(&actions), watch::BrowserMode::Watch);
        assert!(
            matches!(&actions[0], UiAction::LoadThreads { descendant_root: None, filter, .. } if filter.is_empty())
        );
        let mut fresh = state();
        assert_eq!(
            mode(&fresh.event(key(KeyCode::Char('t'), KeyModifiers::CONTROL))),
            watch::BrowserMode::List
        );
    }

    #[test]
    fn f2_captures_entry_time_once_and_all_exits_release_it() {
        let mut ui = state();
        assert!(ui.selection_frozen_at.is_none());
        ui.event(key(KeyCode::F(2), KeyModifiers::NONE));
        let captured = ui
            .selection_frozen_at
            .as_deref()
            .expect("F2 captured its entry time");
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(captured)
                .unwrap()
                .offset()
                .local_minus_utc(),
            0
        );
        // A deterministic older entry avoids sleeps and does not depend on the
        // wall clock advancing to another whole second while this test runs.
        let fixed = "2000-01-02T03:04:05Z";
        ui.selection_frozen_at = Some(fixed.into());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut ui))
            .unwrap();
        let banner = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .take(100)
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(banner.contains(fixed), "{banner}");
        ui.apply(UiUpdate::Status {
            thread_id: None,
            text: "new mutable fact while fixed".into(),
        });
        ui.event(Event::Resize(90, 24));
        assert_eq!(ui.selection_frozen_at.as_deref(), Some(fixed));
        ui.event(key(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert!(ui.full_redraw_requested);
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut ui))
            .unwrap();
        assert_eq!(
            ui.selection_frozen_at.as_deref(),
            Some(fixed),
            "redrawing does not recapture entry time"
        );
        ui.event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!ui.selection_mode);
        assert!(ui.selection_frozen_at.is_none());
        ui.event(key(KeyCode::F(2), KeyModifiers::NONE));
        assert!(ui.selection_mode);
        assert_ne!(ui.selection_frozen_at.as_deref(), Some(fixed));
        assert!(ui.selection_frozen_at.is_some());
        ui.event(Event::Paste("selection-mode paste".into()));
        assert!(!ui.selection_mode);
        assert!(ui.selection_frozen_at.is_none());
    }
    #[test]
    fn f1_dispatch_keeps_operation_field_help_and_layered_general_help_distinct() {
        use ratatui::{Terminal, backend::TestBackend};
        for (width, height) in [(80, 24), (140, 40)] {
            let mut config = state().config;
            config.ui.language = crate::config::UiLanguage::Ja;
            let mut ui = UiState::new(&config, PathBuf::from("/"));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            ui.event(key(KeyCode::F(8), KeyModifiers::NONE));
            ui.event(Event::Paste("run get".into()));
            ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
            ui.event(key(KeyCode::F(1), KeyModifiers::NONE));
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut ui))
                .unwrap();
            let explanation = rendered(&terminal);
            assert!(
                explanation.contains("作業 | 詳細 | 使い方"),
                "{explanation}"
            );
            assert!(explanation.contains("Run ID"), "{explanation}");
            let hint = explanation.lines().last().unwrap();
            assert!(
                !hint.contains("Enter") && !hint.contains("次の項目"),
                "{hint}"
            );
            assert!(!ui.help_visible, "global help must not steal form F1");
            ui.event(key(KeyCode::Esc, KeyModifiers::NONE));
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut ui))
                .unwrap();
            let form = rendered(&terminal);
            assert!(
                form.lines().last().unwrap().contains("Enter 表示"),
                "{form}"
            );
            assert!(
                form.contains("Enter 表示") && !form.contains("使い方"),
                "{form}"
            );
            ui.event(key(KeyCode::Esc, KeyModifiers::NONE));
            ui.event(key(KeyCode::F(1), KeyModifiers::NONE));
            assert!(ui.help_visible && ui.help_above_popups);
            assert!(
                ui.operations.is_some(),
                "list is retained beneath general help"
            );
            ui.event(key(KeyCode::Esc, KeyModifiers::NONE));
            assert_eq!(ui.top_overlay(), Some(Overlay::Operations));
        }
    }
    #[test]
    fn f1_above_client_confirmation_neither_executes_nor_drops_the_choice() {
        let mut ui = state();
        for event in [
            key(KeyCode::F(8), KeyModifiers::NONE),
            Event::Paste("config init".into()),
            key(KeyCode::Enter, KeyModifiers::NONE),
            key(KeyCode::Enter, KeyModifiers::NONE),
        ] {
            assert!(ui.event(event).is_empty());
        }
        assert_eq!(ui.hint_context(), key_hints::Context::Confirmation);
        assert!(ui.event(key(KeyCode::F(1), KeyModifiers::NONE)).is_empty());
        assert!(ui.help_visible && ui.help_above_popups);
        assert!(ui.event(key(KeyCode::Esc, KeyModifiers::NONE)).is_empty());
        assert_eq!(ui.hint_context(), key_hints::Context::Confirmation);
        assert!(
            ui.event(key(KeyCode::Char('n'), KeyModifiers::NONE))
                .is_empty()
        );
        assert_eq!(
            ui.hint_context(),
            key_hints::Context::Form(crate::operations::EnterVerb::Execute)
        );
    }
    #[test]
    fn derived_owner_keeps_valid_stacking_without_extra_mode_state() {
        let mut ui = state();
        assert!(ui.composer_active());
        ui.thread_browser.open(String::new(), vec![], None);
        assert_eq!(ui.top_overlay(), Some(Overlay::Threads));
        ui.help_visible = true;
        assert_eq!(ui.top_overlay(), Some(Overlay::Help));
        ui.management.open_text(ManagementView {
            title: "fixture".into(),
            text: String::new(),
            scroll: 0,
        });
        assert_eq!(ui.top_overlay(), Some(Overlay::Management));
        ui.details = Some(details::Details::new(ui.view(), false));
        assert_eq!(ui.top_overlay(), Some(Overlay::Details));
        ui.operations = Some(operations::Operations::new(Default::default()));
        assert_eq!(ui.top_overlay(), Some(Overlay::Operations));
        ui.open_models("");
        assert_eq!(ui.top_overlay(), Some(Overlay::Models));
        ui.selection_mode = true;
        assert_eq!(ui.top_overlay(), Some(Overlay::Selection));
        ui.confirmation = Some(Confirmation::ForceQuit);
        assert_eq!(ui.top_overlay(), Some(Overlay::Confirmation));
        ui.lifecycle.begin();
        assert_eq!(ui.top_overlay(), Some(Overlay::Shutdown));
        assert!(!ui.composer_active());
        assert!(!ui.history_mouse_active());
        assert!(!ui.history_links_active());
        assert!(
            ui.thread_browser.visible()
                && ui.help_visible
                && ui.management.is_some()
                && ui.details.is_some()
                && ui.operations.is_some()
                && ui.models.is_some(),
            "derivation does not collapse the valid underlying layers"
        );
    }
    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }
    fn install(ui: &mut UiState, owner: Overlay) {
        match owner {
            Overlay::Threads => {
                ui.thread_browser.open(String::new(), vec![], None);
            }
            Overlay::Help => ui.help_visible = true,
            Overlay::Queue => ui.queue_popup.visible = true,
            Overlay::Management => ui.management.open_text(ManagementView {
                title: "fixture".into(),
                text: String::new(),
                scroll: 0,
            }),
            Overlay::Details => ui.details = Some(details::Details::new(ui.view(), false)),
            Overlay::Operations => {
                ui.open_operations(Some(operations::OpKind::ThreadGet));
            }
            Overlay::Models => {
                let request = ui.open_models("");
                let id = match request[0] {
                    UiAction::LoadModels { request_id, .. } => request_id,
                    _ => panic!("model request"),
                };
                ui.apply(UiUpdate::Models {
                    request_id: id,
                    completed_at: chrono::Utc::now(),
                    result: Ok(vec!["model-a".into(), "LEAK".into()]),
                });
            }
            Overlay::Selection => ui.selection_mode = true,
            Overlay::Confirmation => {
                ui.confirmation = Some(Confirmation::Operation {
                    command: Command::ThreadDelete(crate::protocol::ThreadDeleteArgs {
                        preview_marker: None,
                        thread_id: "thread".into(),
                        descendants: false,
                    }),
                    detail: "fixture".into(),
                })
            }
            Overlay::Shutdown => ui.lifecycle.begin(),
        }
    }
    const OWNERS: [Overlay; 9] = [
        Overlay::Threads,
        Overlay::Help,
        Overlay::Management,
        Overlay::Details,
        Overlay::Operations,
        Overlay::Models,
        Overlay::Selection,
        Overlay::Confirmation,
        Overlay::Shutdown,
    ];
    fn command(action: &UiAction) -> Option<&Command> {
        match action {
            UiAction::ManagementIntent { action, .. }
            | UiAction::SubmittedCommand { action, .. } => command(action),
            UiAction::Manage(command) | UiAction::Command(command) => Some(command),
            _ => None,
        }
    }
    #[test]
    fn policy_global_keys_have_explicit_priority_over_each_overlay() {
        for owner in OWNERS {
            let mut ui = state();
            ui.select_thread("thread".into());
            install(&mut ui, owner);
            let actions = ui.event(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
            assert_eq!(
                actions
                    .iter()
                    .any(|a| matches!(a,UiAction::Cancel{thread_id} if thread_id=="thread")),
                owner != Overlay::Shutdown,
                "Ctrl+C {owner:?}"
            );
            let actions = ui.event(key(KeyCode::Char('q'), KeyModifiers::CONTROL));
            assert_eq!(
                actions.iter().any(|a| matches!(a, UiAction::Quit)),
                owner != Overlay::Shutdown,
                "Ctrl+Q {owner:?}"
            );
            let mut ui = state();
            install(&mut ui, owner);
            ui.event(key(KeyCode::F(2), KeyModifiers::NONE));
            assert_eq!(
                ui.selection_mode,
                if owner == Overlay::Selection {
                    false
                } else {
                    !matches!(owner, Overlay::Confirmation | Overlay::Shutdown)
                },
                "F2 {owner:?}"
            );
        }
    }
    #[test]
    fn policy_fold_shortcuts_never_change_a_popup_background_draft() {
        for owner in std::iter::once(None).chain(OWNERS.map(Some)) {
            for code in [KeyCode::F(3), KeyCode::F(4), KeyCode::F(5)] {
                let mut ui = state();
                ui.insert(&"pasted 日本語\n".repeat(100), true);
                assert_eq!(ui.view().draft.folds.len(), 1);
                if let Some(owner) = owner {
                    install(&mut ui, owner);
                }
                let before = ui.view().draft.clone();
                ui.event(key(code, KeyModifiers::NONE));
                assert_eq!(
                    ui.view().draft != before,
                    owner.is_none(),
                    "{code:?} {owner:?}"
                );
            }
        }
    }
    #[test]
    fn policy_paste_cannot_reach_a_form_below_confirmation_shutdown_or_selection() {
        for lower in [Overlay::Models, Overlay::Operations] {
            for upper in [Overlay::Confirmation, Overlay::Shutdown, Overlay::Selection] {
                let mut ui = state();
                ui.select_thread("thread".into());
                install(&mut ui, lower);
                install(&mut ui, upper);
                let draft = ui.view().draft.clone();
                ui.event(Event::Paste("LEAK".into()));
                assert_eq!(ui.view().draft, draft);
                ui.confirmation = None;
                ui.lifecycle = Default::default();
                ui.selection_mode = false;
                let actions = ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
                let result = actions.iter().find_map(command).expect("form result");
                match result {
                    Command::ThreadGet(args) => assert_eq!(
                        args.thread_id.as_deref(),
                        Some("thread"),
                        "{lower:?} under {upper:?}"
                    ),
                    Command::ThreadSet(args) => assert_eq!(
                        args.model.as_deref(),
                        Some("model-a"),
                        "{lower:?} under {upper:?}"
                    ),
                    _ => panic!("unexpected form result {result:?}"),
                }
            }
        }
    }
    #[test]
    fn policy_composer_completion_and_selection_use_matching_mouse_and_link_guards() {
        let mut ui = state();
        ui.insert("/s", false);
        assert!(ui.composer_active() && ui.has_completions());
        assert!(!ui.history_mouse_active() && !ui.history_links_active());
        ui.clear_current_draft();
        ui.selection_mode = true;
        assert!(!ui.composer_active());
        assert!(!ui.history_mouse_active() && !ui.history_links_active());
    }
    #[test]
    fn policy_reader_navigation_keeps_existing_function_key_owners() {
        for owner in std::iter::once(None).chain(OWNERS.map(Some)) {
            for code in [KeyCode::F(6), KeyCode::F(7), KeyCode::F(8)] {
                let mut ui = state();
                if let Some(owner) = owner {
                    install(&mut ui, owner);
                }
                ui.event(key(code, KeyModifiers::NONE));
                let expected = if owner.is_none_or(|o| {
                    matches!(o, Overlay::Threads | Overlay::Help | Overlay::Management)
                }) {
                    if code == KeyCode::F(8) {
                        Some(Overlay::Operations)
                    } else {
                        Some(Overlay::Details)
                    }
                } else {
                    owner
                };
                assert_eq!(ui.top_overlay(), expected, "{code:?} {owner:?}");
            }
        }
    }
    #[test]
    fn selection_suppresses_actual_detail_link_targets_without_discarding_the_popup() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut ui = state();
        ui.select_thread("thread".into());
        ui.apply(UiUpdate::History {
            pending: Default::default(),
            read_revision: 0,
            thread_id: "thread".into(),
            generation: ui.view().generation,
            nodes: vec![UiNode {
                node_id: "link-node".into(),
                role: "assistant".into(),
                text: "saved".into(),
                base_char_offset: 0,
                total_chars: 5,
                reasoning_ranges: vec![],
                presentation: NodePresentation {
                    links: vec![crate::presentation::PresentationLink {
                        label: "fixture link".into(),
                        url: "https://example.invalid/exact?keep=all".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            }],
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 0,
            total_nodes: 1,
        });
        ui.details = Some(details::Details::new(ui.view(), true));
        let mut terminal = Terminal::new(TestBackend::new(90, 28)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        assert!(
            !ui.visible_links.is_empty(),
            "the visible detail really contains a clickable target"
        );
        ui.event(key(KeyCode::F(2), KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        assert!(ui.selection_mode && ui.details.is_some());
        assert!(
            ui.visible_links.is_empty(),
            "native text selection must not retain underlying clickable targets"
        );
    }
    fn rendered(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        let mut text = String::new();
        for y in area.y..area.bottom() {
            let mut x = area.x;
            while x < area.right() {
                let symbol = buffer[(x, y)].symbol();
                text.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            text.push('\n');
        }
        text
    }
    #[test]
    fn rejected_paste_notice_is_bilingual_and_does_not_insert_text() {
        fn configure(ui: &mut UiState) {
            let config = ui.config.clone();
            ui.show_help(help::Page::guide(&config));
            ui.event(Event::Paste("PRIVATE-PASTE".into()));
            assert!(!ui.view().draft.text.contains("PRIVATE-PASTE"));
        }
        fn expected(language: crate::config::UiLanguage) -> &'static [&'static str] {
            match language {
                crate::config::UiLanguage::En => &["Denied:"],
                crate::config::UiLanguage::Ja => &["未反映:"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }
    #[test]
    fn english_rejection_keeps_its_outcome_at_eight_columns() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut ui = state();
        ui.config.ui.language = crate::config::UiLanguage::En;
        ui.show_help(help::Page::guide(&ui.config.clone()));
        let draft = ui.view().draft.clone();
        ui.event(Event::Resize(8, 5));
        ui.event(Event::Paste("PRIVATE-PASTE".into()));
        assert_eq!(ui.view().draft, draft);
        let mut terminal = Terminal::new(TestBackend::new(8, 5)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let screen = rendered(&terminal);
        assert!(
            screen.lines().last().unwrap().contains("Denied"),
            "{screen}"
        );
        assert!(!screen.contains("PRIVATE-PASTE"));
    }
    #[test]
    fn rejected_paste_is_visible_for_every_blocked_owner_without_changing_input() {
        use ratatui::{Terminal, backend::TestBackend};
        for owner in [
            Overlay::Threads,
            Overlay::Help,
            Overlay::Management,
            Overlay::Details,
            Overlay::Confirmation,
            Overlay::Selection,
            Overlay::Shutdown,
        ] {
            for (width, height) in [(8, 5), (100, 25)] {
                let mut ui = state();
                ui.config.ui.language = crate::config::UiLanguage::Ja;
                ui.insert("retained draft", false);
                install(&mut ui, owner);
                let draft = ui.view().draft.clone();
                assert!(
                    ui.event(Event::Paste("PRIVATE PASTE MUST NOT APPEAR".into()))
                        .is_empty()
                );
                assert_eq!(ui.view().draft, draft, "{owner:?}");
                ui.apply(UiUpdate::Status {
                    thread_id: None,
                    text: "background update must not erase rejection".into(),
                });
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
                let screen = rendered(&terminal);
                assert!(screen.contains("未反映"), "{owner:?}: {screen}");
                assert!(!screen.contains("PRIVATE PASTE MUST NOT APPEAR"));
                if owner == Overlay::Selection {
                    assert!(!ui.selection_mode);
                    if width >= 100 {
                        assert!(screen.contains("選択モードを解除"));
                    }
                }
            }
        }
    }
    #[test]
    fn paste_feedback_keeps_status_and_fixed_escape_footer_at_small_and_wide_sizes() {
        use ratatui::{Terminal, backend::TestBackend};
        for (width, height) in [(8, 5), (12, 8), (40, 12), (80, 24), (160, 60)] {
            let mut ui = state();
            ui.config.ui.language = crate::config::UiLanguage::Ja;
            ui.show_help(help::Page::info(
                "fixture reader",
                "fixture",
                format!("{}END_MARKER", "reader row\n".repeat(120)),
            ));
            ui.event(Event::Resize(width, height));
            ui.event(Event::Paste("not inserted".into()));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
            let screen = rendered(&terminal);
            let lines = screen.lines().collect::<Vec<_>>();
            assert!(
                lines[usize::from(height - 1)].contains("未反映"),
                "{width}x{height}: {screen}"
            );
            let footer = lines[..usize::from(popup::viewport(ui.viewport).height)]
                .iter()
                .position(|line| line.contains("Esc"))
                .expect("Escape remains visible above status");
            for _ in 0..200 {
                ui.event(key(KeyCode::PageDown, KeyModifiers::NONE));
            }
            ui.event(Event::Paste("still not inserted".into()));
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
            let screen = rendered(&terminal);
            let lines = screen.lines().collect::<Vec<_>>();
            assert!(
                lines[footer].contains("Esc"),
                "footer moved: {width}x{height}: {screen}"
            );
            assert!(lines[usize::from(height - 1)].contains("未反映"));
            if width >= 40 && height >= 12 {
                assert!(
                    screen.contains("END_MARKER"),
                    "reader cannot reach end: {width}x{height}: {screen}"
                );
            }
            ui.event(key(KeyCode::Esc, KeyModifiers::NONE));
            assert!(!ui.help_visible && ui.input_notice.is_none());
        }
    }
    #[test]
    fn editable_owners_still_accept_paste_and_next_key_clears_only_the_notice() {
        let mut ui = state();
        ui.insert("before", false);
        ui.event(Event::Paste("accepted".into()));
        assert_eq!(ui.view().draft.text, "beforeaccepted");
        assert!(ui.input_notice.is_none());
        for owner in [Overlay::Models, Overlay::Operations] {
            let mut ui = state();
            ui.select_thread("thread".into());
            install(&mut ui, owner);
            ui.event(Event::Paste("LEAK".into()));
            assert!(ui.input_notice.is_none());
            let actions = ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
            match actions.iter().find_map(command).unwrap() {
                Command::ThreadGet(args) => {
                    assert!(args.thread_id.as_ref().unwrap().contains("LEAK"))
                }
                Command::ThreadSet(args) => assert_eq!(args.model.as_deref(), Some("LEAK")),
                other => panic!("unexpected editable owner {other:?}"),
            }
        }
        let mut ui = state();
        install(&mut ui, Overlay::Help);
        ui.event(Event::Paste("ignored".into()));
        ui.apply(UiUpdate::Status {
            thread_id: None,
            text: "saved worker result".into(),
        });
        assert!(ui.input_notice.is_some());
        assert_eq!(
            ui.view().operation_notice.as_ref().unwrap().text,
            "saved worker result"
        );
        ui.event(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(ui.help_visible && ui.input_notice.is_none());
        assert!(ui.view().operation_notice.is_none());
        assert_eq!(
            ui.view().status,
            ui.view().state,
            "a deliberate next input clears the prior concrete operation notice"
        );
    }
    #[test]
    fn tiny_frame_settles_and_clears_obsolete_links_without_an_idle_repaint() {
        use ratatui::{Terminal, backend::TestBackend};
        for (width, height) in [(1, 1), (7, 24), (80, 4)] {
            let mut ui = state();
            ui.visible_links.push(crate::terminal_links::LinkRegion {
                x: 0,
                y: 0,
                width: 1,
                url: "https://example.test/old".into(),
            });
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
            assert!(!ui.dirty, "tiny frame {width}x{height} must settle");
            assert!(
                ui.visible_links.is_empty(),
                "old viewport must not remain clickable"
            );
            ui.event(Event::Resize(80, 24));
            assert!(ui.dirty, "a later resize must redraw");
        }
    }
    #[test]
    fn ignored_events_never_request_a_frame_or_erase_an_existing_request() {
        use crossterm::event::{MouseButton, MouseEvent};
        let mut ui = state();
        ui.view_mut().navigation.scroll.area = Some(Rect::new(5, 5, 20, 10));
        let mouse = |kind, column, row| {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        let ignored = [
            mouse(MouseEventKind::Moved, 10, 10),
            mouse(MouseEventKind::Down(MouseButton::Left), 10, 10),
            mouse(MouseEventKind::Up(MouseButton::Left), 10, 10),
            mouse(MouseEventKind::ScrollDown, 0, 0),
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )),
            Event::FocusGained,
            Event::FocusLost,
        ];
        for event in ignored {
            for dirty in [false, true] {
                ui.dirty = dirty;
                assert!(ui.event(event.clone()).is_empty());
                assert_eq!(ui.dirty, dirty, "{event:?}");
            }
        }
        ui.dirty = false;
        ui.event(mouse(MouseEventKind::ScrollDown, 10, 10));
        assert!(ui.dirty, "accepted wheel still paints the changed viewport");
        ui.dirty = false;
        install(&mut ui, Overlay::Help);
        ui.dirty = false;
        ui.event(mouse(MouseEventKind::ScrollDown, 10, 10));
        assert!(!ui.dirty, "blocked wheel must not redraw the popup");
    }
}
pub(super) fn shutdown_title(language: crate::config::UiLanguage) -> &'static str {
    local!(language, "終了処理", "Shutdown")
}
pub(super) fn shutdown_context(language: crate::config::UiLanguage) -> &'static str {
    local!(language, "保存と実行停止の状態", "Save and stop status")
}
pub(super) fn shutdown_footer(language: crate::config::UiLanguage) -> &'static str {
    local!(
        language,
        "W / Enter 待機継続 | F 強制終了を選択",
        "W / Enter Keep waiting | F Choose force quit"
    )
}
pub(super) fn shutdown_starting(language: crate::config::UiLanguage) -> &'static str {
    local!(
        language,
        "終了処理を開始しています。保存・停止を待っています",
        "Starting shutdown. Waiting for saves and work to stop."
    )
}
pub(super) fn shutdown_progress(
    language: crate::config::UiLanguage,
    elapsed_ms: u64,
    detail: &str,
    force: bool,
) -> String {
    use crate::config::UiLanguage;
    match (language, force) {
        (UiLanguage::Ja, true) => format!(
            "終了処理中 {elapsed_ms}ms\n{detail}\nW/Enter: 待機継続  F: 強制終了を選択（保存/回収未完了の可能性）"
        ),
        (UiLanguage::Ja, false) => {
            format!("終了処理中 {elapsed_ms}ms\n{detail}\n正常終了の保存・停止を待っています")
        }
        (UiLanguage::En, true) => format!(
            "Shutting down {elapsed_ms}ms\n{detail}\nW/Enter: Keep waiting  F: Choose force quit (saving/cleanup may be incomplete)"
        ),
        (UiLanguage::En, false) => format!(
            "Shutting down {elapsed_ms}ms\n{detail}\nWaiting for saves and work to stop normally"
        ),
    }
}

// The shared confirmation dispatcher falls back for force quit and other owners.
pub(super) fn confirmation_title(
    confirmation: &Confirmation,
    language: crate::config::UiLanguage,
) -> Option<&'static str> {
    (!matches!(confirmation, Confirmation::ForceQuit)).then_some(local!(
        language,
        "実行の確認",
        "Confirm action"
    ))
}
pub(super) fn confirmation_context(
    confirmation: &Confirmation,
    language: crate::config::UiLanguage,
) -> Option<&'static str> {
    (!matches!(confirmation, Confirmation::ForceQuit)).then_some(local!(
        language,
        "内容を確認してください",
        "Review the details"
    ))
}
pub(super) fn confirmation_footer(
    confirmation: &Confirmation,
    language: crate::config::UiLanguage,
) -> Option<&'static str> {
    match confirmation {
        Confirmation::ThreadDeletion(_) | Confirmation::ForceQuit => None,
        Confirmation::StaleIndex { .. } => Some(local!(
            language,
            "N / Esc 戻る | 再確認が必要\n^v/PgUp/PgDn 閲覧",
            "N / Esc Back | Review again\n^v/PgUp/PgDn Scroll"
        )),
        _ => Some(local!(
            language,
            "N / Esc 戻る | Y 実行\n^v/PgUp/PgDn 閲覧",
            "Y Execute | N / Esc Back\n^v/PgUp/PgDn Scroll"
        )),
    }
}
pub(super) fn confirmation_text(
    confirmation: &Confirmation,
    language: crate::config::UiLanguage,
) -> Option<String> {
    use crate::config::UiLanguage;
    match confirmation {
        Confirmation::Watch(target) if target.kind == watch::WatchActionKind::Stop => {
            Some(match language {
                UiLanguage::Ja => format!(
                    "表示中の保存済み対象を停止しますか？\nThread {}\nRun {}\n対象Runと復旧世代を再確認し、別のRunには適用しません。",
                    target.thread_id, target.run_id
                ),
                UiLanguage::En => format!(
                    "Stop the displayed saved target?\nThread {}\nRun {}\nThe target Run and recovery generation are checked again; another Run is never affected.",
                    target.thread_id, target.run_id
                ),
            })
        }
        Confirmation::Watch(target) => Some(match language {
            UiLanguage::Ja => format!(
                "表示中の保存済み対象を再試行しますか？\nThread {}\nRun {}\n対象Runと復旧世代を再確認し、別のRunには適用しません。",
                target.thread_id, target.run_id
            ),
            UiLanguage::En => format!(
                "Retry the displayed saved target?\nThread {}\nRun {}\nThe target Run and recovery generation are checked again; another Run is never affected.",
                target.thread_id, target.run_id
            ),
        }),
        Confirmation::EditStop { run_id, .. } => Some(match language {
            UiLanguage::Ja => format!(
                "旧作業 {run_id} を停止しますか？\n停止の終了状態の確定を待ってから元依頼の直前まで新しい会話を分岐し、編集本文を新規送信します。元依頼・旧会話履歴は変更しません。"
            ),
            UiLanguage::En => format!(
                "Stop prior work {run_id}?\nAfter its final state is confirmed, a new conversation is forked just before the original request and the edited text is sent as a new request. The original request and old history are unchanged."
            ),
        }),
        Confirmation::Index {
            command, detail, ..
        } => {
            let action = match command {
                Command::IndexBackfill(_) => "backfill",
                Command::IndexRetry(_) => "retry",
                Command::IndexRebuild(_) => "rebuild",
                _ => local!(language, "索引操作", "index"),
            };
            let sep = crate::ui::text_width::glyphs::separator();
            Some(match language {
                UiLanguage::Ja => format!("索引処理の実行確認{sep}{action}\n{detail}"),
                UiLanguage::En => format!("Confirm index operation{sep}{action}\n{detail}"),
            })
        }
        Confirmation::StaleIndex { detail } => Some(match language {
            UiLanguage::Ja => format!(
                "確認は古くなりました。実行できません。閉じてプレビューをやり直してください。\n{detail}"
            ),
            UiLanguage::En => format!(
                "This confirmation is stale and cannot execute. Close it and preview again.\n{detail}"
            ),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod confirmation_language_tests {
    use super::*;
    use crate::config::UiLanguage;

    #[test]
    fn index_confirmation_screen_preserves_external_preview_in_both_languages() {
        fn configure(ui: &mut UiState) {
            let args = IndexArgs {
                scope: Some(IndexScope::All),
                ..Default::default()
            };
            let actions = ui.execute_operation(Command::IndexBackfill(args.clone()));
            let attempt = actions
                .iter()
                .find_map(|action| match action {
                    UiAction::IndexPreview { attempt, .. } => Some(*attempt),
                    _ => None,
                })
                .expect("a preview must be requested before execution");
            assert!(
                !actions
                    .iter()
                    .any(|action| matches!(action, UiAction::Manage(Command::IndexBackfill(_))))
            );
            ui.apply(UiUpdate::IndexPreviewReady {
                args: args.clone(),
                attempt,
                text: "OPAQUE-PREVIEW: ABC | 123".into(),
            });
            let Some(Confirmation::Index {
                command, detail, ..
            }) = &ui.confirmation
            else {
                panic!("expected confirmation")
            };
            assert!(matches!(command, Command::IndexBackfill(actual) if actual == &args));
            assert!(
                detail
                    .lines()
                    .any(|line| line == "OPAQUE-PREVIEW: ABC | 123")
            );
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &[
                    "Confirm index operation",
                    "OPAQUE-PREVIEW: ABC | 123",
                    "embedding API charges",
                    "Y Execute",
                ],
                UiLanguage::Ja => &[
                    "索引処理の実行確認",
                    "OPAQUE-PREVIEW: ABC | 123",
                    "embedding APIの費用",
                    "Y 実行",
                ],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn storage_confirmation_screen_keeps_category_and_requires_explicit_y() {
        fn configure(ui: &mut UiState) {
            let args = crate::protocol::StorageArgs {
                category: Some(crate::protocol::StorageCategory::Raw),
            };
            assert!(
                ui.execute_operation(Command::StorageCollect(args))
                    .is_empty()
            );
            let Some(Confirmation::Operation { command, detail }) = &ui.confirmation else {
                panic!("expected confirmation")
            };
            assert!(
                matches!(command, Command::StorageCollect(args) if args.category == Some(crate::protocol::StorageCategory::Raw))
            );
            assert!(detail.contains("category=Some(Raw)"));
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &[
                    "Confirm action",
                    "collection of stored data",
                    "category=Some(Raw)",
                    "Y Execute",
                ],
                UiLanguage::Ja => &[
                    "実行の確認",
                    "保存データの回収",
                    "category=Some(Raw)",
                    "Y 実行",
                ],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn shutdown_progress_preserves_raw_detail_and_force_state_in_both_languages() {
        for language in [UiLanguage::Ja, UiLanguage::En] {
            for force in [false, true] {
                let text = shutdown_progress(language, 1234, "OPAQUE-SHUTDOWN-DETAIL", force);
                assert!(text.contains("OPAQUE-SHUTDOWN-DETAIL") && text.contains("1234ms"));
                let force_text = match language {
                    UiLanguage::Ja => "強制終了を選択",
                    UiLanguage::En => "Choose force quit",
                };
                assert_eq!(text.contains(force_text), force, "{text}");
            }
        }
    }

    #[test]
    fn shutdown_screen_is_bilingual_and_preserves_raw_progress() {
        fn configure(ui: &mut UiState) {
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::CONTROL,
            )));
            ui.apply(UiUpdate::ShutdownProgress {
                elapsed_ms: 1234,
                detail: "OPAQUE-SHUTDOWN-DETAIL".into(),
                force_available: true,
            });
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &[
                    "Shutdown",
                    "1234ms",
                    "OPAQUE-SHUTDOWN-DETAIL",
                    "Keep waiting",
                ],
                UiLanguage::Ja => &["終了処理", "1234ms", "OPAQUE-SHUTDOWN-DETAIL", "待機継続"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn stale_index_confirmation_preserves_detail_and_never_offers_execution() {
        let confirmation = Confirmation::StaleIndex {
            detail: "opaque fixture".into(),
        };
        let text = confirmation_text(&confirmation, UiLanguage::En).unwrap();
        assert!(text.contains("stale") && text.contains("cannot execute"));
        assert!(text.contains("opaque fixture"));
        let footer = confirmation_footer(&confirmation, UiLanguage::En).unwrap();
        assert!(!footer.contains("Y Execute"));
        assert!(footer.contains("N / Esc"));
        assert!(
            confirmation_text(&confirmation, UiLanguage::Ja)
                .unwrap()
                .contains("実行できません")
        );
    }

    #[test]
    fn force_quit_stays_on_the_existing_stage_a_translation() {
        let confirmation = Confirmation::ForceQuit;
        assert!(confirmation_title(&confirmation, UiLanguage::En).is_none());
        assert!(confirmation_context(&confirmation, UiLanguage::En).is_none());
        assert!(confirmation_footer(&confirmation, UiLanguage::En).is_none());
        assert!(confirmation_text(&confirmation, UiLanguage::En).is_none());
    }
}
