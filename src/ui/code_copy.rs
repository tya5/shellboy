//! A transient canonical-code chooser hosted by the existing Details overlay.
use super::*;

pub(crate) fn caption(index: usize, language: &str) -> String {
    let key = key_hints::Action::Code.spec().0;
    if language.is_empty() {
        format!("[{}] コピー · {key}で番号を選ぶ", index + 1)
    } else {
        format!("[{}] コピー · {key}で番号を選ぶ · {language}", index + 1)
    }
}
#[derive(Clone, Debug)]
pub struct CopyBlock {
    pub index: usize,
    pub chars: usize,
    pub preview: String,
}
#[derive(Clone, Debug)]
pub struct CopyOffer {
    pub source_hash: String,
    pub blocks: Vec<CopyBlock>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owner {
    draft: String,
    thread: Option<String>,
    generation: u64,
    focus: Option<ItemKey>,
}
impl Owner {
    fn of(view: &ThreadView) -> Self {
        Self {
            draft: view.draft.draft_id.clone(),
            thread: view.thread_id.clone(),
            generation: view.generation,
            focus: view.items.focus.clone(),
        }
    }
}
pub(super) struct Choices {
    owner: Owner,
    pub cancel: tokio_util::sync::CancellationToken,
    source_hash: Option<String>,
    pub node_id: String,
    request_id: u64,
    prefer: bool,
    selected: usize,
    blocks: Option<Vec<CopyBlock>>,
    error: Option<String>,
}
impl Choices {
    pub fn new(view: &ThreadView, node_id: String, request_id: u64, prefer: bool) -> Self {
        Self {
            owner: Owner::of(view),
            cancel: tokio_util::sync::CancellationToken::new(),
            source_hash: None,
            node_id,
            request_id,
            prefer,
            selected: 0,
            blocks: None,
            error: None,
        }
    }
    pub fn accept(
        &mut self,
        view: &ThreadView,
        request_id: u64,
        node_id: &str,
        result: Result<CopyOffer, String>,
    ) -> Option<UiAction> {
        if self.cancel.is_cancelled()
            || self.blocks.is_some()
            || self.owner != Owner::of(view)
            || self.request_id != request_id
            || self.node_id != node_id
        {
            return None;
        }
        match result {
            Ok(offer) => {
                let CopyOffer {
                    source_hash,
                    blocks,
                } = offer;
                self.source_hash = Some(source_hash.clone());
                let auto = if self.prefer && blocks.len() <= 1 {
                    Some(UiAction::CopySelected {
                        node_id: self.node_id.clone(),
                        code_block: blocks.first().map(|b| b.index),
                        source_hash,
                    })
                } else {
                    None
                };
                self.blocks = Some(blocks);
                auto
            }
            Err(error) => {
                self.error = Some(error);
                None
            }
        }
    }
    pub fn hint_context(&self) -> key_hints::Context {
        match &self.blocks {
            None => key_hints::Context::CodePending,
            Some(blocks) if blocks.is_empty() => key_hints::Context::CodeBody,
            Some(_) => key_hints::Context::CodeChoices,
        }
    }
    pub fn matches(&self, view: &ThreadView) -> bool {
        !self.cancel.is_cancelled() && self.owner == Owner::of(view)
    }
    pub fn key(&mut self, key: KeyEvent) -> details::Outcome {
        let key = key_hints::readonly_key(key);
        if key.code == KeyCode::Esc {
            return details::Outcome::Close;
        }
        let Some(blocks) = &self.blocks else {
            return details::Outcome::None;
        };
        let Some(source_hash) = &self.source_hash else {
            return details::Outcome::None;
        };
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(blocks.len().saturating_sub(1))
            }
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = blocks.len().saturating_sub(1),
            code if key_hints::Action::CopyBody.matches(code) => {
                return details::Outcome::Action(Box::new(UiAction::CopySelected {
                    node_id: self.node_id.clone(),
                    code_block: None,
                    source_hash: source_hash.clone(),
                }));
            }
            KeyCode::Enter | KeyCode::Char('c' | '1'..='9') => {
                let selected = match key.code {
                    KeyCode::Char(ch @ '1'..='9') => (ch as usize) - ('1' as usize),
                    _ => self.selected,
                };
                if let Some(block) = blocks.get(selected) {
                    return details::Outcome::Action(Box::new(UiAction::CopySelected {
                        node_id: self.node_id.clone(),
                        code_block: Some(block.index),
                        source_hash: source_hash.clone(),
                    }));
                }
            }
            _ => {}
        }
        details::Outcome::None
    }
    pub fn wheel(&mut self, delta: isize) -> bool {
        let count = self.blocks.as_ref().map_or(0, Vec::len);
        let old = self.selected;
        self.selected = popup::move_selection(old, delta, count);
        old != self.selected
    }
    pub fn draw(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        paint: &std::cell::Cell<Option<popup::PaintBounds>>,
    ) {
        let body = popup::panel_recorded(
            frame,
            area,
            "コードの塊をコピー",
            "保存された原文から取得 · 表示の折返しはコピーしません",
            match self.hint_context() {
                key_hints::Context::CodePending => "Esc 戻る",
                key_hints::Context::CodeBody => "0 本文全体 · Esc 戻る",
                _ => "↑↓ 選択 · Enter/c コピー · 1-9 番号でコピー\n0 本文全体 · Esc 戻る",
            },
            paint,
        );
        if let Some(error) = &self.error {
            popup::reader(
                frame,
                body,
                &[Line::raw(crate::presentation::sanitize_plain(error, 0).0)],
                0,
            );
        } else if let Some(blocks) = &self.blocks {
            if blocks.is_empty() {
                popup::reader(
                    frame,
                    body,
                    &[Line::raw(
                        "コードの塊はありません。0で本文全体をコピーできます",
                    )],
                    0,
                );
            } else {
                popup::list(
                    frame,
                    body,
                    blocks
                        .iter()
                        .map(|b| {
                            popup::Item::new(format!(
                                "[{}] コピー · {}文字 · {}",
                                b.index + 1,
                                b.chars,
                                b.preview
                            ))
                        })
                        .collect(),
                    self.selected,
                );
            }
        } else {
            popup::reader(frame, body, &[Line::raw("原文を取得中")], 0);
        }
    }
}
impl Drop for Choices {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl UiState {
    pub(super) fn copy_target(&self) -> Option<String> {
        self.view()
            .history_focus_item
            .as_ref()
            .map(|item| item.primary_node_id.clone())
            .or_else(|| self.view().navigation.anchor.node_id.clone())
            .or_else(|| {
                self.view()
                    .nodes()
                    .iter()
                    .rev()
                    .find(|node| node.role == "assistant")
                    .map(|node| node.node_id.clone())
            })
    }
    pub(super) fn choose_code(&mut self, prefer: bool) -> Vec<UiAction> {
        let Some(node_id) = self.copy_target() else {
            return self.command_error("コピー対象の回答がありません");
        };
        self.next_detail_request = self.next_detail_request.wrapping_add(1);
        let request_id = self.next_detail_request;
        let mut details = details::Details::for_node(self.view(), node_id.clone(), false);
        details.copy_choices = Some(Choices::new(
            self.view(),
            node_id.clone(),
            request_id,
            prefer,
        ));
        let cancel = details.copy_choices.as_ref().unwrap().cancel.clone();
        self.details = Some(details);
        vec![UiAction::LoadCopyChoices {
            node_id,
            request_id,
            cancel,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> ThreadView {
        ThreadView::new(&Config::default(), PathBuf::from("/fixture"))
    }
    fn offer(count: usize) -> CopyOffer {
        CopyOffer {
            source_hash: "a".repeat(64),
            blocks: (0..count)
                .map(|index| CopyBlock {
                    index,
                    chars: 2,
                    preview: "code".into(),
                })
                .collect(),
        }
    }
    #[test]
    fn painted_chooser_wheel_moves_only_its_selection_and_keeps_exact_copy_binding() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        use ratatui::{Terminal, backend::TestBackend};
        for (width, height) in [(80, 24), (140, 40)] {
            let mut ui = UiState::new(&Config::default(), PathBuf::from("/fixture"));
            let before = ui.view().navigation.anchor.clone();
            let mut details = details::Details::for_node(ui.view(), "node".into(), false);
            let mut choices = Choices::new(ui.view(), "node".into(), 1, false);
            assert!(!choices.wheel(3), "pending chooser is not scrollable");
            assert!(choices.accept(ui.view(), 1, "node", Ok(offer(8))).is_none());
            details.copy_choices = Some(choices);
            ui.details = Some(details);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| super::super::draw(frame, &mut ui))
                .unwrap();
            let bounds = ui.details.as_ref().unwrap().paint_bounds().unwrap();
            let wheel = |ui: &mut UiState, kind, column, row| {
                assert!(
                    ui.event(Event::Mouse(MouseEvent {
                        kind,
                        column,
                        row,
                        modifiers: KeyModifiers::NONE,
                    }))
                    .is_empty()
                );
            };
            wheel(
                &mut ui,
                MouseEventKind::ScrollUp,
                bounds.body.x,
                bounds.body.y,
            );
            assert!(!ui.dirty);
            wheel(&mut ui, MouseEventKind::ScrollDown, 0, 0);
            assert!(!ui.dirty, "outside wheel does not change the chooser");
            wheel(
                &mut ui,
                MouseEventKind::ScrollDown,
                bounds.body.x,
                bounds.body.y,
            );
            assert!(ui.dirty);
            for _ in 0..3 {
                wheel(
                    &mut ui,
                    MouseEventKind::ScrollDown,
                    bounds.body.x,
                    bounds.body.y,
                );
            }
            ui.dirty = false;
            wheel(
                &mut ui,
                MouseEventKind::ScrollDown,
                bounds.body.x,
                bounds.body.y,
            );
            assert!(!ui.dirty, "last choice is a no-op");
            assert_eq!(ui.view().navigation.anchor, before);
            let actions = ui.event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )));
            assert!(actions.iter().any(|action| matches!(action,
                UiAction::CopySelected { node_id, code_block: Some(7), source_hash }
                if node_id == "node" && source_hash == &"a".repeat(64))));
        }
    }
    #[test]
    fn bare_copy_prefers_a_sole_block_or_body_but_multiple_blocks_wait_for_choice() {
        for n in [0, 1, 2] {
            let view = view();
            let mut choices = Choices::new(&view, "node".into(), 1, true);
            assert_eq!(choices.hint_context(), key_hints::Context::CodePending);
            let action = choices.accept(&view, 1, "node", Ok(offer(n)));
            assert_eq!(
                choices.hint_context(),
                if n == 0 {
                    key_hints::Context::CodeBody
                } else {
                    key_hints::Context::CodeChoices
                }
            );
            match (n, action) {
                (
                    0,
                    Some(UiAction::CopySelected {
                        code_block: None, ..
                    }),
                ) => {}
                (
                    1,
                    Some(UiAction::CopySelected {
                        code_block: Some(0),
                        ..
                    }),
                ) => {}
                (2, None) => {}
                _ => panic!("incorrect preferred copy"),
            }
        }
    }
    #[test]
    fn stale_focus_request_or_thread_never_autocopies_and_drop_cancels_preparation() {
        let mut view = view();
        let mut choices = Choices::new(&view, "node".into(), 1, true);
        assert!(choices.accept(&view, 2, "node", Ok(offer(1))).is_none());
        view.items.focus = Some(ItemKey::Node("other".into()));
        assert!(choices.accept(&view, 1, "node", Ok(offer(1))).is_none());
        let token = choices.cancel.clone();
        drop(choices);
        assert!(token.is_cancelled());
    }
    #[test]
    fn numbered_choice_copies_the_exact_node_block_and_bound_source_hash() {
        let view = view();
        let mut choices = Choices::new(&view, "node".into(), 1, false);
        assert!(choices.accept(&view, 1, "node", Ok(offer(2))).is_none());
        let result = choices.key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        match result {
            details::Outcome::Action(action) => assert!(
                matches!(*action,UiAction::CopySelected {code_block:Some(1),ref source_hash,..} if source_hash==&"a".repeat(64))
            ),
            _ => panic!("missing block action"),
        }
    }
}
