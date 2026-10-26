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
            self.input_notice = None;
        }
        if matches!(&event,Event::Key(key) if key.code==KeyCode::Esc) && self.top_popup().is_none()
        {
            self.management.epoch = self.management.epoch.wrapping_add(1);
        }
        let mut actions = self.event_inner(event);
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
                    _ => vec![],
                }
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.dirty = true;
                self.key(key)
            }
            _ => vec![],
        }
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
        if key.code == KeyCode::F(6) {
            self.details = Some(details::Details::new(self.view(), false));
            return vec![];
        }
        if key.code == KeyCode::F(7) {
            self.details = Some(details::Details::new(self.view(), true));
            return vec![];
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
        if let Some(actions) = self.composer_key(key) {
            return actions;
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
        ui.event(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(ui.help_visible && ui.input_notice.is_none());
        assert_eq!(ui.view().status, "saved worker result");
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
