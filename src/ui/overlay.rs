//! UI input ownership, derived from existing overlay state without replacing stacking.
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
        let usage_before = self.management.usage_snapshot;
        let owner_before = self.management.owner.clone();
        let epoch_before = self.management.epoch;
        let frame_before = self.management.is_some();
        let had_operations = self.operations.is_some();
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
        let follow_before = self.view().follow;
        let mut actions = self.event_inner(event);
        if self.selection_mode != selection_before {
            self.selection_frozen_at = self
                .selection_mode
                .then(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        }
        if self.active == input_owner && self.view().follow != follow_before {
            let view = self.view();
            self.trace_history(crate::diagnostics::UiHistoryTrace {
                reason: crate::diagnostics::UiHistoryReason::UserFollow,
                generation: view.generation,
                view_revision: view.view_revision,
                read_revision: view.delivery_read_revision,
                follow_before,
                follow_after: view.follow,
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
        let closed_operations = had_operations && self.operations.is_none();
        let frame_hidden = frame_before
            && (self.management.is_none() || self.top_overlay() != Some(Overlay::Management));
        let scope_invalidated = owner_before.is_some()
            && (owner_before != self.management.owner || epoch_before != self.management.epoch);
        if usage_before
            && (closes_management || closed_operations || frame_hidden || scope_invalidated)
        {
            if self.management.epoch == epoch_before {
                self.management.epoch = self.management.epoch.wrapping_add(1);
            }
            actions.push(UiAction::CloseUsageDetails {
                epoch: self.management.epoch,
            });
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
                | UiAction::ReloadAttachments { .. }
                | UiAction::IndexPreview(_)
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
        fn usage_read(action: &UiAction) -> bool {
            match action {
                UiAction::SubmittedCommand { action, .. } => usage_read(action),
                UiAction::Command(command) | UiAction::Manage(command) => matches!(
                    command,
                    Command::UsageGet(_)
                        | Command::ThreadGet(_)
                        | Command::RunGet(_)
                        | Command::TurnGet(_)
                ),
                _ => false,
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
                self.management.usage_snapshot = actions.iter().any(usage_read);
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
                        self.input_notice =
                            Some("未反映: 選択モードを解除したので再貼付してください");
                    }
                    Some(Overlay::Shutdown) => {
                        self.input_notice = Some("未反映: 終了処理中は貼付けできません")
                    }
                    Some(Overlay::Confirmation) => {
                        self.input_notice = Some("未反映: 確認を閉じてから再貼付してください")
                    }
                    _ => self.input_notice = Some("未反映: Escで戻って再貼付してください"),
                }
                vec![]
            }
            Event::Resize(width, height) => {
                let viewport = Rect::new(0, 0, width, height);
                self.dirty |= self.viewport != viewport;
                self.viewport = viewport;
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
                    && self.view().scroll.area.is_none_or(|area| {
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
    fn key(&mut self, key: KeyEvent) -> Vec<UiAction> {
        if let Some(actions) = self.global_key(key) {
            return actions;
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
                KeyCode::Char('t') => return self.slash("/threads"),
                KeyCode::Char('n') => return self.new_chat(),
                KeyCode::End => {
                    let view = self.active_view_mut();
                    view.items.active = false;
                    view.live_work.active = false;
                    view.cancel_navigation();
                    view.follow = true;
                    view.unseen = false;
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
                return self.scroll(-(self.view().scroll.height.saturating_sub(1).max(1) as isize));
            }
            KeyCode::PageDown => {
                return self.scroll(self.view().scroll.height.saturating_sub(1).max(1) as isize);
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
                    operations::Outcome::None => vec![],
                }
            }
            Overlay::Details => self.detail_key(key),
            Overlay::Management => self.management_key(key),
            Overlay::Help => {
                let page = popup::body_area(
                    popup::viewport(self.viewport),
                    self.help_page.context,
                    HELP_FOOTER,
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
                    result: Ok(vec!["model-a".into(), "LEAK".into()]),
                });
            }
            Overlay::Selection => ui.selection_mode = true,
            Overlay::Confirmation => {
                ui.confirmation = Some(Confirmation::Operation {
                    command: Command::ThreadDelete(crate::protocol::ThreadDeleteArgs {
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
        ui.view_mut().scroll.area = Some(Rect::new(5, 5, 20, 10));
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

#[cfg(test)]
mod usage_close_tests {
    use super::*;
    #[test]
    fn f8_close_invalidates_a_scoped_usage_cache_without_interrupting_core() {
        let mut ui = UiState::new(
            &crate::config::Config::default(),
            std::path::PathBuf::from("."),
        );
        ui.open_operations(None);
        ui.management.usage_snapshot = true;
        ui.management.owner = Some(ManagementContext {
            epoch: ui.management.epoch,
            owner_draft: ui.view().draft.draft_id.clone(),
            owner_thread: None,
        });
        let actions = ui.event(Event::Key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE)));
        assert!(ui.operations.is_none());
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, UiAction::CloseUsageDetails { .. }))
        );
        assert!(!actions.iter().any(|a| matches!(
            a,
            UiAction::Cancel { .. } | UiAction::Command(Command::RunStop(_))
        )));
    }
}

#[cfg(test)]
mod displayed_usage_close_tests {
    use super::*;
    #[tokio::test]
    async fn real_result_popup_esc_and_f8_close_capture_owner_before_it_is_cleared() {
        for key in [KeyCode::Esc, KeyCode::F(8)] {
            let mut ui = UiState::new(
                &crate::config::Config::default(),
                std::path::PathBuf::from("."),
            );
            let command = UiAction::Command(Command::UsageGet(crate::protocol::UsageGetArgs {
                thread_id: Some("t".into()),
                ..Default::default()
            }));
            let issued = ui.scope_management_actions(vec![command]);
            let context = match &issued[0] {
                UiAction::ManagementIntent { context, .. } => context.clone(),
                _ => panic!("scoped query"),
            };
            let reply = || UiUpdate::ScopedManagement {
                context: context.clone(),
                update: Box::new(UiUpdate::StructuredManagement {
                    work_status: false,
                    request_id: 1,
                    title: "usage".into(),
                    fields: crate::presentation::fields_of_value(
                        &serde_json::json!({"model_attempts":{"completed":{"input_tokens":{"known_sum":10}}}}),
                    ),
                    omitted: false,
                }),
            };
            ui.apply(reply());
            assert_eq!(ui.top_overlay(), Some(Overlay::Management));
            let (send, mut receive) = crate::ui::actions::channel(1);
            send.try_send(issued.into_iter().next().unwrap()).unwrap();
            let actions = ui.event(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)));
            assert!(ui.management.owner.is_none());
            assert!(!ui.management.is_some());
            let close = actions
                .into_iter()
                .find(|a| matches!(a, UiAction::CloseUsageDetails { .. }))
                .expect("actual result closure notifies frontend");
            send.try_send(close).unwrap();
            let closed = match receive.recv_with_admission(false).await.unwrap() {
                UiAction::CloseUsageDetails { epoch } => epoch,
                _ => panic!("priority close"),
            };
            assert_ne!(closed, context.epoch);
            assert!(
                matches!(receive.recv_with_admission(true).await,Some(UiAction::ManagementIntent{context:old,..}) if old==context)
            );
            ui.apply(reply());
            assert!(
                !ui.management.is_some(),
                "late scoped result cannot reopen the closed popup"
            );
        }
    }
}
