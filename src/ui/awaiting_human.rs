//! Pending replies for the active Thread, separated by the existing person/model marks.
use super::region::{RegionHit, RegionLayout, RegionRow};
use super::*;
use crate::config::UiLanguage;
use crate::ui::text_width::glyphs;
use ratatui::style::Style;

fn rows(view: &ThreadView) -> &[ReplyTarget] {
    &view.pending_messages.awaiting_replies
}
pub(super) fn height(available: u16, collapsed: bool, capacity: usize, count: usize) -> u16 {
    if count == 0 {
        return 0;
    }
    RegionLayout::new(capacity, collapsed).height(available, 0)
}
pub(super) fn configure(view: &mut ThreadView, collapsed: bool, capacity: usize, area: Rect) {
    let count = rows(view).len();
    view.awaiting_human.configure(collapsed, capacity);
    view.awaiting_human.set_area(area, count, count > capacity);
    let visible = view.awaiting_human.visible_rows;
    view.awaiting_human.displayed_keys = rows(view)
        .iter()
        .take(visible)
        .map(|row| row.message_id.clone())
        .collect();
    if !view
        .awaiting_human
        .selected
        .as_ref()
        .is_some_and(|key| view.awaiting_human.displayed_keys.contains(key))
    {
        view.awaiting_human.selected = view.awaiting_human.displayed_keys.first().cloned();
    }
    if visible == 0 {
        view.awaiting_human.active = false;
    }
}
pub(super) fn draw(frame: &mut Frame<'_>, area: Rect, view: &ThreadView, language: UiLanguage) {
    if area.height == 0 {
        return;
    }
    let state = &view.awaiting_human;
    let hidden = rows(view).len().saturating_sub(state.visible_rows) as u64;
    let mut title = i18n::local!(language, "返信待ち", "Pending replies").to_owned();
    if rows(view).is_empty() {
        title.push_str(i18n::local!(language, " なし", " None"));
    }
    if state.collapsed {
        title.push_str(i18n::local!(language, " (折り畳み)", " (folded)"));
    }
    if hidden > 0 {
        let suffix = match language {
            UiLanguage::Ja => format!("ほか{hidden}件"),
            UiLanguage::En => format!("{hidden} more"),
        };
        title.push_str(&format!("{}{suffix}", glyphs::separator()));
    }
    let mut lines = super::region::heading_lines(
        area,
        super::region::heading(area.width, &title, view.palette, false),
    );
    for (index, row) in rows(view).iter().take(state.visible_rows).enumerate() {
        let selected = state.active && state.selected.as_deref() == Some(&row.message_id);
        let prefix = super::region::numbered_branch(
            index + 1 == state.visible_rows && hidden == 0,
            index + 1,
            false,
            if row.to_human {
                crate::presentation::rows::RowKind::Human.mark()
            } else {
                crate::presentation::rows::RowKind::OtherModel.mark()
            },
        );
        let metadata = format!(
            "{}{}ms",
            row.created_at.chars().take(32).collect::<String>(),
            row.elapsed_ms
        );
        lines.push(
            RegionRow {
                prefix,
                label: &row.excerpt,
                metadata: &metadata,
                overflow: "",
                selected,
                metadata_style: Style::default().fg(view.palette.muted()),
            }
            .paint(area.width, view.palette),
        );
    }
    if hidden > 0 && usize::from(area.height.saturating_sub(2)) > state.visible_rows {
        let more = match language {
            UiLanguage::Ja => format!("ほか{hidden}件"),
            UiLanguage::En => format!("{hidden} more"),
        };
        lines.push(Line::styled(
            format!("{} {more}", glyphs::branch(true)),
            view.palette.live_work(),
        ));
    }
    state.layout().pad_body(area, &mut lines);
    frame.render_widget(Paragraph::new(lines), area);
}
impl UiState {
    pub(super) fn awaiting_human_click(&mut self, column: u16, row: u16) -> bool {
        match self.view().awaiting_human.hit(column, row) {
            RegionHit::Outside => return false,
            RegionHit::Heading => {
                self.region_owner = super::held_input::RegionOwner::Human;
                self.active_view_mut().held_input.active = false;
                self.toggle_focused_region();
            }
            RegionHit::Row(index) => self.human_select(index),
            RegionHit::Blank => {}
        }
        true
    }
    pub(super) fn human_select(&mut self, index: usize) {
        let key = self
            .view()
            .awaiting_human
            .displayed_keys
            .get(index)
            .cloned();
        let view = self.active_view_mut();
        view.items.active = false;
        view.live_work.region.active = false;
        view.held_input.active = false;
        view.awaiting_human.active = key.is_some();
        view.awaiting_human.selected = key;
        if view.awaiting_human.active {
            self.region_owner = super::held_input::RegionOwner::Human;
        }
        self.dirty = true;
    }
    pub(super) fn human_key(&mut self, key: KeyEvent) -> Option<Vec<UiAction>> {
        if !self.view().awaiting_human.active {
            return None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Tab => self.active_view_mut().awaiting_human.active = false,
            KeyCode::Up | KeyCode::Down => {
                let view = self.view();
                let index = view.awaiting_human.next_index(
                    view.awaiting_human.displayed_keys.iter(),
                    key.code == KeyCode::Down,
                );
                self.human_select(index);
            }
            KeyCode::Enter => {
                let selected = self.view().awaiting_human.selected.as_deref();
                if let Some(target) = rows(self.view())
                    .iter()
                    .find(|item| Some(item.message_id.as_str()) == selected)
                    .cloned()
                {
                    self.active_view_mut().composer.reply = Some(target);
                    self.active_view_mut().awaiting_human.active = false;
                    self.active_view_mut().status = i18n::local!(
                        self.config.ui.language,
                        "判断への返信を入力してください",
                        "Enter your decision"
                    )
                    .into();
                }
            }
            KeyCode::Char('d') => {
                let ids = self
                    .view()
                    .awaiting_human
                    .selected
                    .clone()
                    .into_iter()
                    .collect();
                self.dirty = true;
                return Some(self.cancel_pending_messages(ids));
            }
            KeyCode::Char(' ') | KeyCode::Char('o') | KeyCode::F(6) => {
                if let Some(id) = self.view().awaiting_human.selected.clone() {
                    return Some(if key.code == KeyCode::F(6) {
                        self.open_pending_status(id)
                    } else {
                        self.open_pending_source(id)
                    });
                }
            }
            _ => return None,
        }
        self.dirty = true;
        Some(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_mixed_reply_region_uses_existing_marks() {
        assert_eq!(height(20, false, 1, 0), 0);
        assert_eq!(height(20, false, 1, 1), 3);
        let mut view = ThreadView::new(&Config::default(), std::path::PathBuf::from("/tmp"));
        view.pending_messages.awaiting_replies = vec![
            ReplyTarget {
                message_id: "h".into(),
                sender: "Human decision".into(),
                created_at: "now".into(),
                excerpt: "pick".into(),
                applied: true,
                to_human: true,
                recipient_thread_id: "current".into(),
                elapsed_ms: 0,
            },
            ReplyTarget {
                message_id: "t".into(),
                sender: "worker".into(),
                created_at: "now".into(),
                excerpt: "review".into(),
                applied: true,
                to_human: false,
                recipient_thread_id: "current".into(),
                elapsed_ms: 0,
            },
        ];
        configure(&mut view, false, 2, Rect::new(0, 0, 50, 5));
        assert_eq!(view.awaiting_human.displayed_keys, ["h", "t"]);
    }
}
