//! Saved input presentation. Receipt admission, source loading and I/O stay outside.
use super::region::{RegionHit, RegionLayout};
use super::*;
use crate::config::UiLanguage;
use crate::presentation::rows::RowKind;
use crate::protocol::SenderKind;
use crate::ui::text_width::{UnicodeWidthStr, glyphs};
#[cfg(test)]
use ratatui::style::Modifier;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum HeldKey {
    Saved(String),
    Accepting(u64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RegionOwner {
    Live,
    Held,
}

enum HeldRow<'a> {
    Saved(&'a PendingMessage),
    Accepting(u64, &'a PendingSend),
}
impl HeldRow<'_> {
    fn key(&self) -> HeldKey {
        match self {
            Self::Saved(item) => HeldKey::Saved(item.message_id.clone()),
            Self::Accepting(id, _) => HeldKey::Accepting(*id),
        }
    }
    fn kind(&self) -> RowKind {
        match self {
            Self::Saved(item) if item.completion_scheduled => RowKind::Revisit,
            Self::Saved(item) => match item.sender_kind {
                SenderKind::Human => RowKind::Human,
                SenderKind::Model => RowKind::OtherModel,
                SenderKind::ExternalCli | SenderKind::Unknown => RowKind::External,
            },
            Self::Accepting(_, _) => RowKind::Human,
        }
    }
    fn excerpt(&self) -> String {
        match self {
            Self::Saved(item) => {
                let mut text = message_queue::accepting_excerpt(&item.excerpt);
                if item.excerpt_truncated {
                    text.push_str(glyphs::ellipsis());
                }
                if item.attachment_count > 0 {
                    text.push_str(&format!(
                        "{}[{}]",
                        glyphs::separator(),
                        item.attachment_count
                    ));
                }
                text
            }
            Self::Accepting(_, pending) => message_queue::accepting_excerpt(&pending.snapshot.text),
        }
    }
    fn metadata(&self, language: UiLanguage, width: usize) -> String {
        match self {
            Self::Saved(item) => {
                let label = item.region_metadata(language);
                let timed = format!("{label}{}{}", glyphs::separator(), item.created_at);
                if timed.width() <= width { timed } else { label }
            }
            Self::Accepting(_, pending) if pending.source_node_id.is_some() => {
                i18n::local!(language, "保存済み・取得中", "Saved; loading").into()
            }
            Self::Accepting(_, _) => i18n::local!(language, "送信受付中", "Submitting").into(),
        }
    }
}

fn rows(view: &ThreadView) -> impl Iterator<Item = HeldRow<'_>> {
    view.pending_messages
        .items
        .iter()
        .map(HeldRow::Saved)
        .chain(
            view.pending
                .iter()
                .filter(|(_, pending)| {
                    pending.send.is_some() && pending.status == ReceiptStatus::Accepting
                })
                .map(|(id, pending)| HeldRow::Accepting(*id, pending)),
        )
}
fn total(view: &ThreadView) -> u64 {
    view.pending_messages.total.saturating_add(
        view.pending
            .values()
            .filter(|pending| pending.send.is_some() && pending.status == ReceiptStatus::Accepting)
            .count() as u64,
    )
}

pub(super) fn height(available: u16, collapsed: bool, capacity: usize) -> u16 {
    RegionLayout::new(capacity, collapsed).height(available, 0)
}
pub(super) fn configure(view: &mut ThreadView, collapsed: bool, capacity: usize, area: Rect) {
    let count = rows(view).count();
    let overflow = total(view) > count as u64 || count > capacity;
    view.held_input.configure(collapsed, capacity);
    view.held_input.set_area(area, count, overflow);
    let visible = view.held_input.visible_rows;
    view.held_input.displayed_keys = rows(view).take(visible).map(|row| row.key()).collect();
    if !view
        .held_input
        .selected
        .as_ref()
        .is_some_and(|key| view.held_input.displayed_keys.contains(key))
    {
        view.held_input.selected = view.held_input.displayed_keys.first().cloned();
    }
    if visible == 0 {
        view.held_input.active = false;
    }
}

pub(super) fn draw(frame: &mut Frame<'_>, area: Rect, view: &ThreadView, language: UiLanguage) {
    if area.height == 0 {
        return;
    }
    let state = &view.held_input;
    let total = total(view);
    let hidden = total.saturating_sub(state.visible_rows as u64);
    let title = i18n::local!(language, "保留中の入力", "Held input");
    let empty = if total == 0 {
        i18n::local!(language, " なし", " None")
    } else {
        ""
    };
    let folded = if state.collapsed {
        i18n::local!(language, " (折り畳み)", " (folded)")
    } else {
        ""
    };
    let separate_overflow = !state.collapsed
        && hidden > 0
        && usize::from(area.height.saturating_sub(2)) > state.visible_rows;
    let overflow = if hidden > 0 {
        match language {
            UiLanguage::Ja => format!("ほか{hidden}件"),
            UiLanguage::En => format!("{hidden} more"),
        }
    } else {
        String::new()
    };
    let suffix = if !overflow.is_empty() && !separate_overflow {
        format!("{}{overflow}", glyphs::separator())
    } else {
        String::new()
    };
    let heading = super::region::heading(
        area.width,
        &format!("{title}{empty}{folded}{suffix}"),
        view.palette,
        false,
    );
    let mut lines = super::region::heading_lines(area, heading);
    for (index, row) in rows(view).take(state.visible_rows).enumerate() {
        let selected = state.active && state.selected.as_ref() == Some(&row.key());
        let prefix = super::region::numbered_branch(
            index + 1 == state.visible_rows && !separate_overflow,
            index + 1,
            false,
            row.kind().mark(),
        );
        let metadata = crate::presentation::sanitize_plain(
            &row.metadata(language, usize::from(area.width) / 2),
            0,
        )
        .0
        .replace(['\n', '\r', '\t'], " ");
        lines.push(
            super::region::RegionRow {
                prefix,
                label: &row.excerpt(),
                metadata: &metadata,
                overflow: "",
                selected,
                metadata_style: Style::default(),
            }
            .paint(area.width, view.palette),
        );
    }
    if separate_overflow {
        lines.push(Line::styled(
            format!("{} {overflow}", glyphs::branch(true)),
            view.palette.live_work(),
        ));
    }
    state.layout().pad_body(area, &mut lines);
    frame.render_widget(Paragraph::new(lines), area);
}

impl UiState {
    pub(super) fn toggle_focused_region(&mut self) {
        if self.region_owner == RegionOwner::Live {
            self.toggle_live_work_section();
        } else {
            self.held_input_collapsed = !self.held_input_collapsed;
            let (fold, cap) = (self.held_input_collapsed, self.config.ui.held_input_rows);
            self.active_view_mut().held_input.configure(fold, cap);
            self.dirty = true;
        }
    }
    fn held_select(&mut self, index: usize) {
        let region = &self.view().held_input;
        let key = (!region.collapsed)
            .then(|| region.displayed_keys.get(index).cloned())
            .flatten();
        let view = self.active_view_mut();
        view.items.active = false;
        view.live_work.region.active = false;
        view.held_input.active = key.is_some();
        view.held_input.selected = key;
        if view.held_input.active {
            self.region_owner = RegionOwner::Held;
        }
        self.dirty = true;
    }
    pub(super) fn held_input_click(&mut self, column: u16, row: u16) -> bool {
        match self.view().held_input.hit(column, row) {
            RegionHit::Outside => return false,
            RegionHit::Heading => {
                self.region_owner = RegionOwner::Held;
                self.active_view_mut().live_work.region.active = false;
                self.toggle_focused_region();
            }
            RegionHit::Row(index) => self.held_select(index),
            RegionHit::Blank => {}
        }
        true
    }
    /// Shift+Tab visits each displayed stable identity, then returns to composer.
    pub(super) fn region_key(&mut self, key: KeyEvent) -> Option<Vec<UiAction>> {
        if !self.composer_active()
            || self.has_completions()
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }
        if key.code == KeyCode::BackTab {
            if key_matches(&self.config.ui.send_key, key)
                || self
                    .config
                    .ui
                    .newline_keys
                    .iter()
                    .any(|binding| key_matches(binding, key))
            {
                return None;
            }
            let view = self.view();
            if view.live_work.region.active {
                let index = view
                    .live_work
                    .region
                    .displayed_keys
                    .iter()
                    .position(|key| Some(key) == view.live_work.region.selected.as_ref())
                    .unwrap_or(0);
                if index + 1 < view.live_work.region.visible_rows {
                    self.active_view_mut().live_work.move_focus(true);
                } else {
                    self.active_view_mut().live_work.region.active = false;
                    self.held_select(0);
                }
            } else if view.held_input.active {
                let index = view
                    .held_input
                    .displayed_keys
                    .iter()
                    .position(|key| Some(key) == view.held_input.selected.as_ref())
                    .unwrap_or(0);
                if index + 1 < view.held_input.visible_rows {
                    self.held_select(index + 1);
                } else {
                    self.active_view_mut().held_input.active = false;
                }
            } else if view.live_work.region.visible_rows > 0 {
                let target = view.live_work.region.displayed_keys.first().cloned();
                let view = self.active_view_mut();
                view.items.active = false;
                view.live_work.region.active = true;
                view.live_work.region.selected = target;
                self.region_owner = RegionOwner::Live;
            } else {
                self.held_select(0);
            }
            self.dirty = true;
            return Some(vec![]);
        }
        if !self.view().held_input.active {
            return None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Tab => self.active_view_mut().held_input.active = false,
            KeyCode::Up | KeyCode::Down => {
                let view = self.view();
                let index = view.held_input.next_index(
                    view.held_input.displayed_keys.iter(),
                    key.code == KeyCode::Down,
                );
                self.held_select(index);
            }
            KeyCode::Char(' ') | KeyCode::Char('o') | KeyCode::Enter | KeyCode::F(6) => {
                let selected = self.view().held_input.selected.clone();
                let Some(HeldKey::Saved(id)) = selected else {
                    return Some(vec![]);
                };
                if !self
                    .view()
                    .held_input
                    .displayed_keys
                    .contains(&HeldKey::Saved(id.clone()))
                {
                    return Some(vec![]);
                }
                return Some(
                    if matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('o')) {
                        self.open_pending_source(id)
                    } else {
                        self.open_pending_status(id)
                    },
                );
            }
            KeyCode::Char('d' | 'a') => {
                let all = key.code == KeyCode::Char('a');
                let selected = self.view().held_input.selected.as_ref();
                let ids = self
                    .view()
                    .held_input
                    .displayed_keys
                    .iter()
                    .filter_map(|key| match key {
                        HeldKey::Saved(id) if all || selected == Some(key) => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                self.confirm_pending_cancel(ids);
            }
            KeyCode::F(7) => {}
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete => {}
            _ => return None,
        }
        self.dirty = true;
        Some(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn item(id: &str) -> PendingMessage {
        PendingMessage {
            message_id: id.into(),
            operation_id: format!("op:{id}"),
            sender_thread: None,
            sender_kind: SenderKind::Human,
            completion_scheduled: false,
            created_at: "2026-11-04T00:00:00Z".into(),
            held: true,
            hold_reason: Some("recovery_wait".into()),
            uncertain: false,
            now: false,
            excerpt: format!("synthetic {id}"),
            excerpt_truncated: false,
            attachment_count: 0,
            expect_reply: false,
        }
    }
    fn ui() -> UiState {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        ui.active_view_mut().pending_messages = PendingMessages {
            items: vec![item("a"), item("b"), item("c"), item("d")],
            total: 4,
            ..Default::default()
        };
        configure(ui.active_view_mut(), false, 3, Rect::new(0, 5, 80, 5));
        ui
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn screen(view: &ThreadView, width: u16, language: UiLanguage) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, 5)).unwrap();
        terminal
            .draw(|frame| draw(frame, frame.area(), view, language))
            .unwrap();
        terminal.backend().buffer().clone()
    }
    #[test]
    fn empty_regions_do_not_gain_focus_and_configured_backtab_keeps_priority() {
        let mut ui = ui();
        ui.active_view_mut().pending_messages = Default::default();
        configure(ui.active_view_mut(), false, 3, Rect::new(0, 0, 80, 5));
        ui.key(key(KeyCode::BackTab));
        assert!(!ui.view().held_input.active);
        assert_eq!(ui.region_owner, RegionOwner::Live);
        ui.config.ui.newline_keys.push("BackTab".into());
        ui.key(key(KeyCode::BackTab));
        assert_eq!(ui.view().draft.text, "\n");
        assert!(!ui.view().held_input.active);
    }
    #[test]
    fn saved_origin_marks_do_not_infer_human_from_null_thread() {
        let mut saved = item("a");
        for (kind, expected) in [
            (SenderKind::Human, RowKind::Human),
            (SenderKind::Model, RowKind::OtherModel),
            (SenderKind::ExternalCli, RowKind::External),
            (SenderKind::Unknown, RowKind::External),
        ] {
            saved.sender_kind = kind;
            assert_eq!(HeldRow::Saved(&saved).kind(), expected);
        }
        saved.completion_scheduled = true;
        assert_eq!(HeldRow::Saved(&saved).kind(), RowKind::Revisit);
    }
    #[test]
    fn fixed_empty_heading_both_languages_and_fold_geometry() {
        let mut ui = ui();
        ui.active_view_mut().pending_messages = Default::default();
        for (language, title) in [(UiLanguage::En, "None"), (UiLanguage::Ja, "なし")] {
            configure(ui.active_view_mut(), false, 3, Rect::new(0, 0, 80, 5));
            let buffer = screen(ui.view(), 80, language);
            let mut text = String::new();
            let mut x = 0;
            while x < buffer.area.width {
                let symbol = buffer[(x, 1)].symbol();
                text.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            assert!(text.contains(title));
            assert_eq!(ui.view().held_input.visible_rows, 0);
            assert_eq!(height(20, false, 3), 5);
            assert_eq!(height(20, true, 3), 2);
        }
    }
    #[test]
    fn focus_visits_displayed_rows_then_composer_and_fold_keeps_owner() {
        let mut ui = ui();
        assert_eq!(ui.view().held_input.visible_rows, 2);
        ui.region_key(key(KeyCode::BackTab));
        assert_eq!(
            ui.view().held_input.selected,
            Some(HeldKey::Saved("a".into()))
        );
        assert!(ui.view().held_input.active);
        ui.region_key(key(KeyCode::BackTab));
        assert_eq!(
            ui.view().held_input.selected,
            Some(HeldKey::Saved("b".into()))
        );
        ui.region_key(key(KeyCode::BackTab));
        assert!(!ui.view().held_input.active);
        assert!(ui.held_input_click(0, 6));
        assert!(ui.held_input_collapsed);
        ui.toggle_focused_region();
        assert!(!ui.held_input_collapsed);
        assert!(!ui.live_work_collapsed);
    }
    #[test]
    fn source_and_details_use_existing_async_saved_id_actions() {
        let mut ui = ui();
        ui.held_select(1);
        let actions = ui.region_key(key(KeyCode::F(6))).unwrap();
        assert!(
            matches!(&actions[..], [UiAction::Manage(Command::MessageGet(target))] if target.message_id == "b")
        );
        ui.management = Default::default();
        let actions = ui.region_key(key(KeyCode::Char(' '))).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(ui.details.is_some());
    }
    #[test]
    fn cancellation_freezes_exact_painted_ids_across_unpainted_refresh() {
        let mut ui = ui();
        ui.held_select(0);
        ui.active_view_mut().pending_messages.items = vec![item("new-unpainted")];
        ui.region_key(key(KeyCode::Char('a')));
        let Some(Confirmation::MessageQueue { action, .. }) = ui.confirmation else {
            panic!()
        };
        assert!(
            matches!(*action, UiAction::CancelMessages { message_ids, .. } if message_ids == ["a", "b"])
        );
    }
    #[test]
    fn selected_line_is_reverse_bold_even_without_color_and_no_caret() {
        let mut ui = ui();
        ui.active_view_mut().palette =
            theme::Palette::from_capabilities(crate::config::PaletteMode::Dark, true, true);
        ui.held_select(0);
        for width in [18, 80, 120] {
            let buffer = screen(ui.view(), width, UiLanguage::En);
            for x in 0..width {
                let cell = &buffer[(x, 2)];
                assert!(cell.modifier.contains(Modifier::REVERSED | Modifier::BOLD));
            }
            let text: String = (0..width).map(|x| buffer[(x, 2)].symbol()).collect();
            // The only human mark is the saved-origin mark, not a focus caret.
            assert_eq!(text.matches(RowKind::Human.mark()).count(), 1);
        }
    }
    #[test]
    fn time_is_omitted_before_reason_is_shortened_and_excerpts_stay_bounded() {
        let saved = item("a");
        let row = HeldRow::Saved(&saved);
        assert!(!row.metadata(UiLanguage::En, 18).contains(&saved.created_at));
        assert!(
            row.metadata(UiLanguage::En, 18)
                .contains("Waiting for recovery")
        );
        assert!(
            row.metadata(UiLanguage::En, 120)
                .contains(&saved.created_at)
        );
        let mut huge = saved;
        huge.excerpt = "界".repeat(100_000);
        assert!(HeldRow::Saved(&huge).excerpt().chars().count() <= 513);
    }
    #[test]
    fn cycle_crosses_live_then_held_and_popup_has_input_ownership() {
        use super::super::live_work::{LiveWorkRow, LiveWorkSnapshot, LiveWorkTarget};
        let mut ui = ui();
        let at = Instant::now();
        let rows = ["one", "two"]
            .into_iter()
            .map(|id| LiveWorkRow {
                target: LiveWorkTarget::Shell {
                    execution_id: id.into(),
                    turn_id: None,
                },
                label: format!("synthetic {id}"),
                description: None,
                state: "running".into(),
                started_at: None,
                elapsed_base: None,
                last_output_at: None,
                wait_info: None,
                output_age_base: None,
            })
            .collect();
        ui.active_view_mut().live_work.accept(
            Some(LiveWorkSnapshot {
                run_id: "run".into(),
                rows,
                shells: 2,
                children: 0,
                waits: Some(0),
                overflow: 0,
                counts_capped: false,
                observed_at: at,
            }),
            at,
        );
        ui.active_view_mut().live_work.configure(false, 3);
        ui.active_view_mut()
            .live_work
            .set_area(Rect::new(0, 0, 80, 5));
        for id in ["one", "two"] {
            ui.key(key(KeyCode::BackTab));
            assert_eq!(ui.region_owner, RegionOwner::Live);
            assert!(
                matches!(ui.view().live_work.region.selected.as_ref(), Some(LiveWorkTarget::Shell { execution_id, .. }) if execution_id == id)
            );
        }
        for id in ["a", "b"] {
            ui.key(key(KeyCode::BackTab));
            assert_eq!(
                ui.view().held_input.selected,
                Some(HeldKey::Saved(id.into()))
            );
            assert!(!ui.view().live_work.region.active);
        }
        ui.key(key(KeyCode::BackTab));
        assert!(!ui.view().held_input.active);
        ui.queue_popup.visible = true;
        ui.key(key(KeyCode::BackTab));
        assert!(!ui.view().held_input.active);
        assert!(!ui.view().live_work.region.active);
    }
    #[test]
    fn full_layout_orders_regions_and_keeps_headings_when_empty() {
        let mut ui = ui();
        ui.active_view_mut().pending_messages = Default::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, &mut ui))
            .unwrap();
        let live = ui.view().live_work.region.area.unwrap();
        let held = ui.view().held_input.area.unwrap();
        assert_eq!(held.y, live.bottom());
        assert!(held.bottom() < 30);
        let view = ui.view();
        assert_eq!(view.live_work.region.visible_rows, 0);
        assert_eq!(view.held_input.visible_rows, 0);
        assert_eq!(live.height, 5);
        assert_eq!(held.height, 5);
    }
    #[test]
    fn accepting_rows_never_clone_or_render_full_drafts_or_invent_saved_ids() {
        let mut ui = ui();
        ui.active_view_mut().pending_messages = Default::default();
        ui.insert("synthetic send", false);
        ui.key(key(KeyCode::Enter));
        let pending = ui.active_view_mut().pending.values_mut().next().unwrap();
        pending.snapshot.text = format!(
            "VISIBLE\x1b[2J {} TAIL_NOT_DISPLAYED",
            "x".repeat(4 * 1024 * 1024)
        );
        configure(ui.active_view_mut(), false, 3, Rect::new(0, 0, 80, 5));
        let displayed = ui.view().held_input.displayed_keys.first().cloned();
        assert!(matches!(displayed, Some(HeldKey::Accepting(_))));
        ui.held_select(0);
        assert!(ui.region_key(key(KeyCode::F(6))).unwrap().is_empty());
        ui.region_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(ui.confirmation.is_none());
        let buffer = screen(ui.view(), 80, UiLanguage::En);
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("VISIBLE"));
        assert!(!text.contains("TAIL_NOT_DISPLAYED"));
        assert!(!text.contains('\x1b'));
    }
}
