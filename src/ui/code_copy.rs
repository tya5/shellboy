//! A transient canonical-code chooser hosted by the existing Details overlay.
use super::*;
#[derive(Clone, Debug)]
pub struct CopyBlock {
    pub index: usize,
    pub chars: usize,
    pub preview: String,
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
        Self { draft: view.draft.draft_id.clone(), thread: view.thread_id.clone(),
            generation: view.generation, focus: view.items.focus.clone() }
    }
}
pub(super) struct Choices {
    owner: Owner,
    pub node_id: String,
    request_id: u64,
    prefer: bool,
    selected: usize,
    blocks: Option<Vec<CopyBlock>>,
    error: Option<String>,
}
impl Choices {
    pub fn new(view: &ThreadView, node_id: String, request_id: u64, prefer: bool) -> Self {
        Self { owner: Owner::of(view), node_id, request_id, prefer, selected: 0, blocks: None, error: None }
    }
    pub fn accept(&mut self, view: &ThreadView, request_id: u64, node_id: &str,
        result: Result<Vec<CopyBlock>, String>) -> Option<UiAction> {
        if self.owner != Owner::of(view) || self.request_id != request_id || self.node_id != node_id { return None; }
        match result {
            Ok(blocks) => {
                let auto = if self.prefer && blocks.len() <= 1 {
                    Some(UiAction::Copy { node_id: self.node_id.clone(), code_block: blocks.first().map(|b| b.index) })
                } else { None };
                self.blocks = Some(blocks);
                auto
            }
            Err(error) => { self.error = Some(error); None }
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> details::Outcome {
        let key = key_hints::readonly_key(key);
        if key.code == KeyCode::Esc { return details::Outcome::Close; }
        let Some(blocks) = &self.blocks else { return details::Outcome::None; };
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(blocks.len().saturating_sub(1)),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = blocks.len().saturating_sub(1),
            KeyCode::Char('0') => return details::Outcome::Action(Box::new(UiAction::Copy { node_id: self.node_id.clone(), code_block: None })),
            KeyCode::Enter | KeyCode::Char('c' | '1'..='9') => {
                let selected = match key.code { KeyCode::Char(ch @ '1'..='9') => (ch as usize) - ('1' as usize), _ => self.selected };
                if let Some(block) = blocks.get(selected) {
                    return details::Outcome::Action(Box::new(UiAction::Copy { node_id: self.node_id.clone(), code_block: Some(block.index) }));
                }
            }
            _ => {}
        }
        details::Outcome::None
    }
    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect) {
        let body = popup::panel(frame, area, "コードの塊をコピー", "保存された原文から取得 · 表示の折返しはコピーしません",
            "↑↓ 選択 · Enter/c コピー · 1-9 番号でコピー\n0 本文全体 · Esc 戻る");
        if let Some(error) = &self.error {
            popup::reader(frame, body, &[Line::raw(crate::presentation::sanitize_plain(error,0).0)], 0);
        } else if let Some(blocks) = &self.blocks {
            if blocks.is_empty() {
                popup::reader(frame, body, &[Line::raw("コードの塊はありません。0で本文全体をコピーできます")], 0);
            } else {
                popup::list(frame, body, blocks.iter().map(|b| popup::Item::text(format!("[{}] コピー · {}文字 · {}",b.index+1,b.chars,b.preview))).collect(), self.selected);
            }
        } else { popup::reader(frame, body, &[Line::raw("原文を取得中")], 0); }
    }
}
impl UiState {
    pub(super) fn copy_target(&self) -> Option<String> {
        self.view().history_focus_item.as_ref().map(|item| item.primary_node_id.clone())
            .or_else(|| self.view().anchor.node_id.clone())
            .or_else(|| self.view().nodes().iter().rev().find(|node| node.role == "assistant").map(|node| node.node_id.clone()))
    }
    pub(super) fn choose_code(&mut self, prefer: bool) -> Vec<UiAction> {
        let Some(node_id) = self.copy_target() else { return self.command_error("コピー対象の回答がありません"); };
        self.next_detail_request = self.next_detail_request.wrapping_add(1);
        let request_id = self.next_detail_request;
        let mut details = details::Details::for_node(self.view(), node_id.clone(), false);
        details.copy_choices = Some(Choices::new(self.view(), node_id.clone(), request_id, prefer));
        self.details = Some(details);
        vec![UiAction::LoadCopyChoices { node_id, request_id }]
    }
}
