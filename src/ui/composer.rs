//! Completion is local; prompt recall is a bounded asynchronous DB read.
use super::i18n::local;
use super::theme;
use super::*;
use std::collections::VecDeque;

// Named text keys keep this local UI copy ready for the #35 translation layer.
#[derive(Clone, Copy)]
enum ComposerText {
    LiteralPasteWarning,
    RetypeCommandHint,
}
impl ComposerText {
    fn text(self, language: crate::config::UiLanguage) -> &'static str {
        match self {
            Self::LiteralPasteWarning => local!(
                language,
                "貼り付けた文として送ります（コマンドは実行しません）",
                "This pasted text will be sent as a message, not run as a command."
            ),
            Self::RetypeCommandHint => local!(
                language,
                "実行するには先頭の /コマンド名 を消して手で入力してください",
                "To run a command, remove the leading slash and type it yourself."
            ),
        }
    }
}

impl DraftSnapshot {
    pub(super) fn command_is_typed(&self) -> bool {
        if !self.literal_paste {
            return true;
        }
        if !self.text.starts_with('/') {
            return false;
        }
        let Some(prefix) = self.text.get(..self.typed_prefix) else {
            return false;
        };
        // Inspect only the trusted prefix, not an arbitrarily long pasted tail.
        prefix.contains(char::is_whitespace)
            || self.typed_prefix == self.text.len()
            || self.text[self.typed_prefix..].starts_with(char::is_whitespace)
    }
}

fn recalled_input_status(language: crate::config::UiLanguage) -> &'static str {
    crate::ui::text_width::glyphs::select(
        local!(
            language,
            "入力履歴 · ↑古い / ↓新しい・下書きへ",
            "Input history · ↑ older / ↓ newer · Down returns to draft"
        ),
        local!(
            language,
            "入力履歴 | ^古い / v新しい・下書きへ",
            "Input history | ^ older / v newer | Down returns to draft"
        ),
    )
}

fn completion_hint(language: crate::config::UiLanguage) -> &'static str {
    crate::ui::text_width::glyphs::select(
        local!(
            language,
            "↑↓ 選択 · Tab/Enter 補完 · Esc 閉じる",
            "↑↓ Select · Tab/Enter complete · Esc close"
        ),
        local!(
            language,
            "^v 選択 | Tab/Enter 補完 | Esc 閉じる",
            "^v Select | Tab/Enter complete | Esc close"
        ),
    )
}

fn input_history_error(language: crate::config::UiLanguage, error: &str) -> String {
    match language {
        crate::config::UiLanguage::Ja => format!("入力履歴エラー: {error}"),
        crate::config::UiLanguage::En => format!("Input history error: {error}"),
    }
}

struct CommandHint {
    name: &'static str,
}
impl CommandHint {
    fn help(&self, language: crate::config::UiLanguage) -> &'static str {
        match self.name {
            "/agents" => local!(
                language,
                "指示ファイル参照を確認／変更",
                "Inspect or change instruction file"
            ),
            "/received" => local!(
                language,
                "受信記録（失敗・ストリーム差分）を読む",
                "Read received records and stream diffs"
            ),
            "/status" => local!(
                language,
                "作業状態・背景コマンド・子の作業を確認",
                "Inspect work, background shells, and child Runs"
            ),
            "/help" => local!(language, "操作ガイド", "Usage guide"),
            "/new" => local!(language, "新しい会話", "New conversation"),
            "/threads" => local!(language, "会話一覧", "List conversations"),
            "/threads active" => local!(language, "作業を優先順に監視", "Watch work by priority"),
            "/threads descendants" => local!(
                language,
                "現在の会話の子孫を監視",
                "Watch descendants of this conversation"
            ),
            "/switch" => local!(language, "IDで会話を切替", "Switch conversation by ID"),
            "/model" => local!(
                language,
                "接続・モデルを確認／変更",
                "Inspect or change connection and model"
            ),
            "/models" => local!(
                language,
                "接続先のモデル一覧・選択",
                "List and select models for the connection"
            ),
            "/effort" => local!(language, "推論強度を変更", "Change reasoning effort"),
            "/cwd" => local!(
                language,
                "作業ディレクトリを確認／変更",
                "Inspect or change working directory"
            ),
            "/attach" => local!(language, "ファイルを添付", "Attach a file"),
            "/attachments" => local!(language, "添付一覧", "List attachments"),
            "/detach" => local!(language, "添付を外す", "Remove an attachment"),
            "/reload-attachments" => local!(language, "添付を再読込", "Reload attachments"),
            "/image" => local!(
                language,
                "クリップボードの画像を下書きに添付",
                "Attach clipboard image to draft"
            ),
            "/paste-image" => local!(language, "画像を貼付け（別名）", "Paste image (alias)"),
            "/search" => local!(language, "履歴を検索", "Search history"),
            "/index" => local!(language, "検索索引を管理", "Manage search index"),
            "/fork" => local!(
                language,
                "履歴から会話を分岐",
                "Fork conversation from history"
            ),
            "/edit" => local!(language, "依頼を編集して分岐", "Edit request and fork"),
            "/branches" => local!(language, "分岐の木を表示", "Show the branch tree"),
            "/rewind" => local!(
                language,
                "依頼の前まで戻す（残りは子会話）",
                "Rewind to before a request (rest kept as a child)"
            ),
            "/compact-max" => local!(
                language,
                "APIなしで最大縮小",
                "Maximum compaction without API"
            ),
            "/compact" => local!(language, "会話を圧縮", "Compact conversation"),
            "/details" => local!(
                language,
                "履歴の詳細・原文",
                "History details and original text"
            ),
            "/links" => local!(language, "完全URL一覧", "List full URLs"),
            "/actions" => local!(language, "操作フォーム", "Operation forms"),
            "/operations" => local!(language, "操作フォーム（別名）", "Operation forms (alias)"),
            "/copy" => local!(
                language,
                "原文・コードをコピー",
                "Copy original text or code"
            ),
            "/reasoning" => local!(language, "推論の表示を切替", "Toggle reasoning display"),
            "/select" => local!(language, "端末の選択モード", "Terminal selection mode"),
            "/paste" => local!(
                language,
                "貼付けblockの確認・編集",
                "Review or edit pasted block"
            ),
            "/restore" => local!(
                language,
                "入力範囲を戻す。番号付きは失敗した下書きを復元",
                "Restore input range; numbered form restores a failed draft"
            ),
            "/retry" => local!(
                language,
                "現在の復旧待ちを同じ内容で再試行",
                "Retry current recovery with the same content"
            ),
            "/stop" => local!(
                language,
                "現在の会話の作業を停止",
                "Stop work in this conversation"
            ),
            "/cancel-wait" => local!(language, "待機の観測を取消", "Cancel wait observation"),
            "/cancel" => local!(language, "実行を中断", "Interrupt execution"),
            "/background" => local!(
                language,
                "foreground shellを背景化",
                "Move foreground shell to background"
            ),
            "/quit" => local!(language, "通常終了", "Exit normally"),
            "/replies" => local!(
                language,
                "返事待ちを確認して返信",
                "Inspect awaiting replies and reply"
            ),
            "/queue" => local!(
                language,
                "保存済みの反映待ち・保留入力を確認／取消",
                "Inspect or cancel queued and held input"
            ),
            _ => "",
        }
    }
}

const COMMANDS: &[CommandHint] = &[
    CommandHint { name: "/agents" },
    CommandHint { name: "/received" },
    CommandHint { name: "/status" },
    CommandHint { name: "/help" },
    CommandHint { name: "/new" },
    CommandHint { name: "/threads" },
    CommandHint {
        name: "/threads active",
    },
    CommandHint {
        name: "/threads descendants",
    },
    CommandHint { name: "/switch" },
    CommandHint { name: "/model" },
    CommandHint { name: "/models" },
    CommandHint { name: "/effort" },
    CommandHint { name: "/cwd" },
    CommandHint { name: "/attach" },
    CommandHint {
        name: "/attachments",
    },
    CommandHint { name: "/detach" },
    CommandHint {
        name: "/reload-attachments",
    },
    CommandHint { name: "/image" },
    CommandHint {
        name: "/paste-image",
    },
    CommandHint { name: "/search" },
    CommandHint { name: "/index" },
    CommandHint { name: "/fork" },
    CommandHint { name: "/edit" },
    CommandHint { name: "/rewind" },
    CommandHint { name: "/branches" },
    CommandHint {
        name: "/compact-max",
    },
    CommandHint { name: "/compact" },
    CommandHint { name: "/details" },
    CommandHint { name: "/links" },
    CommandHint { name: "/actions" },
    CommandHint {
        name: "/operations",
    },
    CommandHint { name: "/copy" },
    CommandHint { name: "/reasoning" },
    CommandHint { name: "/select" },
    CommandHint { name: "/paste" },
    CommandHint { name: "/restore" },
    CommandHint { name: "/retry" },
    CommandHint { name: "/stop" },
    CommandHint {
        name: "/cancel-wait",
    },
    CommandHint { name: "/cancel" },
    CommandHint {
        name: "/background",
    },
    CommandHint { name: "/quit" },
    CommandHint { name: "/queue" },
    CommandHint { name: "/replies" },
];

#[derive(Debug, Default)]
pub(super) struct Composer {
    pub(super) reply: Option<message_queue::ReplyTarget>,
    pub(super) reply_submission: Option<(u64, String)>,
    selection: Option<(String, usize)>,
    dismissed: Option<String>,
    recall: Option<Recall>,
}
#[derive(Debug)]
struct Recall {
    original: DraftSnapshot,
    ordinal: Option<u64>,
    node_id: Option<String>,
    shown_revision: u64,
    pending: Option<(u64, bool)>,
    queued: VecDeque<RecallKey>,
}
#[derive(Debug, PartialEq, Eq)]
enum RecallKey {
    Move(bool),
    Submit,
}
impl Composer {
    pub(super) fn edited(&mut self) -> bool {
        let recalled = self.recall.take().is_some();
        self.selection = None;
        self.dismissed = None;
        recalled
    }
    fn cancel_request(&mut self) -> bool {
        if let Some(recall) = self.recall.as_mut() {
            let pending = recall.pending.take().is_some();
            recall.queued.clear();
            return pending;
        }
        false
    }
    pub(super) fn stop_recall(&mut self) {
        self.recall = None;
    }
}

impl UiState {
    pub(super) fn cancel_input_recall(&mut self) {
        let view = self.active_view_mut();
        if view.composer.cancel_request() {
            view.status = local!(view.language, "入力できます", "Ready for input").into();
        }
    }
    pub(super) fn has_completions(&self) -> bool {
        !self.completions().is_empty()
    }
    fn completions(&self) -> Vec<&'static CommandHint> {
        let draft = &self.view().draft;
        if !draft.command_is_typed()
            || draft.cursor != draft.text.len()
            || !draft.text.starts_with('/')
            || self.view().composer.dismissed.as_ref() == Some(&draft.text)
        {
            return vec![];
        }
        let mut candidates: Vec<_> = COMMANDS
            .iter()
            .filter(|c| c.name.starts_with(&draft.text))
            .collect();
        // A complete command must be the default choice even when a longer
        // command was registered first. Explicit arrow selection still wins.
        candidates.sort_by_key(|candidate| candidate.name != draft.text);
        candidates
    }
    fn completion_index(&self, count: usize) -> usize {
        self.view()
            .composer
            .selection
            .as_ref()
            .filter(|(query, _)| query == &self.view().draft.text)
            .map_or(0, |(_, index)| *index)
            .min(count.saturating_sub(1))
    }
    pub(super) fn composer_key(&mut self, key: KeyEvent) -> Option<Vec<UiAction>> {
        if key_matches(&self.config.ui.send_key, key)
            && self
                .view()
                .composer
                .recall
                .as_ref()
                .is_some_and(|r| r.pending.is_some())
        {
            self.queue_recall_key(RecallKey::Submit);
            return Some(vec![]);
        }
        if key.modifiers != KeyModifiers::NONE {
            return None;
        }
        let candidates = self.completions();
        if !candidates.is_empty() {
            let selected = self.completion_index(candidates.len());
            match key.code {
                KeyCode::Up | KeyCode::Down => {
                    let index = if key.code == KeyCode::Up {
                        (selected + candidates.len() - 1) % candidates.len()
                    } else {
                        (selected + 1) % candidates.len()
                    };
                    let query = self.view().draft.text.clone();
                    self.active_view_mut().composer.selection = Some((query, index));
                    return Some(vec![]);
                }
                KeyCode::Tab | KeyCode::Enter => {
                    let name = candidates[selected].name;
                    if key.code == KeyCode::Enter && self.view().draft.text == name {
                        return None;
                    }
                    let completed = format!("{name} ");
                    if completed.len() as u64 > self.config.limits.draft_bytes {
                        let language = self.view().language;
                        self.active_view_mut().status = local!(
                            language,
                            "補完結果が下書きのbyte上限を超えます",
                            "The completion exceeds the draft byte limit."
                        )
                        .into();
                        return Some(vec![]);
                    }
                    self.begin_edit();
                    let draft = &mut self.active_view_mut().draft;
                    draft.text = completed;
                    draft.cursor = draft.text.len();
                    draft.folds.clear();
                    draft.literal_paste = false;
                    draft.typed_prefix = draft.text.len();
                    return Some(vec![]);
                }
                KeyCode::Esc => {
                    let query = self.view().draft.text.clone();
                    self.active_view_mut().composer.dismissed = Some(query);
                    return Some(vec![]);
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                let view = self.view();
                let browsing = view.composer.recall.is_some();
                let draft = &view.draft;
                let at_edge = if key.code == KeyCode::Up {
                    !draft.text[..draft.cursor].contains('\n')
                } else {
                    !draft.text[draft.cursor..].contains('\n')
                };
                if browsing || at_edge {
                    return Some(self.recall_input(key.code == KeyCode::Up));
                }
            }
            _ => {}
        }
        None
    }
    fn recall_input(&mut self, older: bool) -> Vec<UiAction> {
        let Some(thread_id) = self.view().thread_id.clone() else {
            return vec![];
        };
        if self
            .view()
            .pending
            .get(&self.view().draft.revision)
            .is_some_and(|p| p.status == ReceiptStatus::Accepting)
        {
            return vec![];
        }
        let view = self.active_view_mut();
        if view
            .composer
            .recall
            .as_ref()
            .is_some_and(|r| r.shown_revision != view.draft.revision)
        {
            view.composer.stop_recall();
        }
        if view.composer.recall.is_none() {
            if !older {
                return vec![];
            }
            view.composer.recall = Some(Recall {
                original: view.draft.clone(),
                ordinal: None,
                node_id: None,
                shown_revision: view.draft.revision,
                pending: None,
                queued: VecDeque::new(),
            });
        }
        let recall = view.composer.recall.as_mut().unwrap();
        if recall.pending.is_some() {
            self.queue_recall_key(RecallKey::Move(older));
            return vec![];
        }
        if !older && recall.ordinal.is_none() {
            self.restore_recall_draft();
            return vec![];
        }
        self.next_input_request = self.next_input_request.wrapping_add(1);
        let request_id = self.next_input_request;
        let view = self.active_view_mut();
        let recall = view.composer.recall.as_mut().unwrap();
        recall.pending = Some((request_id, older));
        view.status = local!(view.language, "入力履歴を読込み中", "Loading input history").into();
        vec![UiAction::RecallInput {
            thread_id,
            generation: view.generation,
            request_id,
            before_ordinal: recall.ordinal,
            older,
            before_node_id: recall.node_id.clone(),
        }]
    }
    fn restore_recall_draft(&mut self) {
        let view = self.active_view_mut();
        if let Some(recall) = view.composer.recall.take() {
            let revision = view.draft.revision.wrapping_add(1);
            view.draft = recall.original;
            view.draft.revision = revision;
            view.status = local!(
                view.language,
                "編集中の下書きに戻りました",
                "Restored the draft being edited"
            )
            .into();
        }
    }
    fn queue_recall_key(&mut self, key: RecallKey) {
        let view = self.active_view_mut();
        let Some(recall) = view.composer.recall.as_mut() else {
            return;
        };
        if recall.queued.len() < 256 {
            recall.queued.push_back(key);
        } else {
            view.status = local!(
                view.language,
                "入力履歴の操作待ちが上限です。読み込み後にもう一度操作してください",
                "The input-history action queue is full. Try again after loading completes."
            )
            .into();
        }
    }
    pub(super) fn apply_recalled_input(
        &mut self,
        thread_id: String,
        generation: u64,
        request_id: u64,
        result: Result<Option<RecalledPrompt>, String>,
    ) {
        if self.view().thread_id.as_ref() != Some(&thread_id)
            || self.view().generation != generation
        {
            return;
        }
        let view = self.active_view_mut();
        let Some(recall) = view.composer.recall.as_mut() else {
            return;
        };
        if recall.pending.map(|(id, _)| id) != Some(request_id) {
            return;
        }
        if recall.shown_revision != view.draft.revision {
            view.composer.stop_recall();
            return;
        }
        let (_, older) = recall.pending.take().unwrap();
        match result {
            Ok(Some(RecalledPrompt {
                ordinal,
                node_id,
                text,
            })) => {
                if text.len() as u64 > self.config.limits.draft_bytes {
                    let language = self.view().language;
                    self.active_view_mut().status = local!(
                        language,
                        "入力履歴が下書き上限を超えます。本文は変更していません",
                        "The recalled input exceeds the draft limit; the draft was not changed."
                    )
                    .into();
                    self.active_view_mut()
                        .composer
                        .recall
                        .as_mut()
                        .unwrap()
                        .queued
                        .clear();
                    return;
                }
                let fold_id = self.next_fold;
                self.next_fold = self.next_fold.wrapping_add(1);
                let fold = text.chars().count() > self.config.ui.paste_fold_chars
                    || text.lines().count() > self.config.ui.paste_fold_lines;
                let view = self.active_view_mut();
                view.draft.text = text;
                // Recall replaces the whole text. Retained unsent references
                // append to that new text; Down restores the original snapshot.
                for attachment in &mut view.draft.attachments {
                    attachment.position = None;
                }
                view.draft.cursor = view.draft.text.len();
                view.draft.folds = if fold {
                    vec![PasteFold {
                        id: fold_id,
                        start: 0,
                        end: view.draft.text.len(),
                        expanded: false,
                    }]
                } else {
                    vec![]
                };
                view.draft.literal_paste = true;
                view.draft.typed_prefix = 0;
                view.draft.revision = view.draft.revision.wrapping_add(1);
                let recall = view.composer.recall.as_mut().unwrap();
                recall.ordinal = Some(ordinal);
                recall.node_id = Some(node_id);
                recall.shown_revision = view.draft.revision;
                view.status = recalled_input_status(view.language).into();
            }
            Ok(None) if !older => {
                let queued = std::mem::take(&mut recall.queued);
                self.restore_recall_draft();
                self.replay_recall_queue(queued);
                return;
            }
            Ok(None) => {
                while recall.queued.front() == Some(&RecallKey::Move(true)) {
                    recall.queued.pop_front();
                }
                view.status = local!(
                    view.language,
                    "これより古い入力はありません",
                    "No older input is available."
                )
                .into();
            }
            Err(error) => {
                recall.queued.clear();
                recall.ordinal = None;
                recall.node_id = None;
                view.status = input_history_error(view.language, &error);
                return;
            }
        }
        let queued = std::mem::take(
            &mut self
                .active_view_mut()
                .composer
                .recall
                .as_mut()
                .unwrap()
                .queued,
        );
        self.replay_recall_queue(queued);
    }
    fn replay_recall_queue(&mut self, mut queued: VecDeque<RecallKey>) {
        while let Some(key) = queued.pop_front() {
            let actions = match key {
                RecallKey::Move(older) => self.recall_input(older),
                RecallKey::Submit if self.composer_active() => {
                    let actions = self.submit();
                    self.scope_management_actions(actions)
                }
                RecallKey::Submit => Vec::new(),
            };
            self.deferred_actions.extend(actions);
            if let Some(recall) = self.active_view_mut().composer.recall.as_mut()
                && recall.pending.is_some()
            {
                recall.queued.extend(queued);
                return;
            }
        }
    }
    pub(super) fn draw_completions(&self, frame: &mut Frame<'_>, input: Rect) {
        if !self.composer_active() {
            return;
        }
        let draft = &self.view().draft;
        if draft.text.starts_with('/') && !draft.command_is_typed() {
            let height = 2.min(input.y);
            let warning = Rect::new(
                input.x + 1,
                input.y - height,
                input.width.saturating_sub(2),
                height,
            );
            frame.render_widget(Clear, warning);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(ComposerText::LiteralPasteWarning.text(self.view().language)),
                    Line::from(ComposerText::RetypeCommandHint.text(self.view().language)),
                ])
                .style(self.palette.header()),
                warning,
            );
            return;
        }
        let candidates = self.completions();
        if candidates.is_empty() {
            return;
        }
        let count = candidates
            .len()
            .min(6)
            .min(input.y.saturating_sub(2) as usize);
        if count == 0 {
            return;
        }
        let selected = self.completion_index(candidates.len());
        let first = selected.saturating_sub(count - 1);
        let popup = Rect::new(
            input.x + 1,
            input.y - count as u16 - 1,
            input.width.saturating_sub(2),
            count as u16 + 1,
        );
        frame.render_widget(Clear, popup);
        let mut lines = candidates[first..first + count]
            .iter()
            .enumerate()
            .map(|(index, hint)| {
                let active = first + index == selected;
                Line::from(format!(
                    "{} {:<21} {}",
                    if active { "›" } else { " " },
                    hint.name,
                    hint.help(self.view().language)
                ))
                .style(
                    Style::default()
                        .fg(if active { theme::ACCENT } else { theme::TEXT })
                        .bg(theme::DEFAULT_BACKGROUND),
                )
            })
            .collect::<Vec<_>>();
        lines.push(
            Line::from(completion_hint(self.view().language))
                .style(Style::default().fg(theme::MUTED)),
        );
        frame.render_widget(Paragraph::new(lines), popup);
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    #[test]
    fn command_hints_are_localized_without_changing_command_names() {
        for hint in COMMANDS {
            let ja = hint.help(crate::config::UiLanguage::Ja);
            let en = hint.help(crate::config::UiLanguage::En);
            assert!(!ja.is_empty(), "{}", hint.name);
            assert!(!en.is_empty(), "{}", hint.name);
            assert!(en.is_ascii(), "{}: {}", hint.name, en);
        }
        assert_eq!(
            COMMANDS.iter().map(|hint| hint.name).collect::<Vec<_>>()[0],
            "/agents"
        );
    }

    #[test]
    fn recall_and_completion_hints_follow_language_without_changing_glyph_fallbacks() {
        for (language, expected) in [
            (crate::config::UiLanguage::Ja, "入力履歴"),
            (crate::config::UiLanguage::En, "Input history"),
        ] {
            assert!(recalled_input_status(language).contains(expected));
        }
        for (language, expected) in [
            (crate::config::UiLanguage::Ja, "選択"),
            (crate::config::UiLanguage::En, "Select"),
        ] {
            assert!(completion_hint(language).contains(expected));
        }
    }

    #[test]
    fn dynamic_input_history_error_is_complete_in_both_languages() {
        let error = "synthetic reason";
        assert_eq!(
            input_history_error(crate::config::UiLanguage::Ja, error),
            "入力履歴エラー: synthetic reason"
        );
        assert_eq!(
            input_history_error(crate::config::UiLanguage::En, error),
            "Input history error: synthetic reason"
        );
    }

    #[test]
    fn composer_warnings_have_complete_english_and_japanese_text() {
        for text in [
            ComposerText::LiteralPasteWarning,
            ComposerText::RetypeCommandHint,
        ] {
            assert!(!text.text(crate::config::UiLanguage::Ja).is_empty());
            assert!(text.text(crate::config::UiLanguage::En).is_ascii());
        }
    }

    fn enter(ui: &mut UiState) -> Vec<UiAction> {
        ui.event(crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
    }
    #[test]
    fn pasted_arguments_do_not_disable_a_manually_typed_command() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("/switch ", false);
        ui.insert("synthetic-thread", true);
        assert!(ui.view().draft.literal_paste);
        assert!(ui.view().draft.command_is_typed());
        assert!(
            enter(&mut ui).iter().any(|action| matches!(action, UiAction::SelectThread { thread_id, .. } if thread_id == "synthetic-thread"))
        );
    }
    #[test]
    fn pasting_first_then_typing_the_complete_name_at_home_is_explicit() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("synthetic-thread", true);
        ui.active_view_mut().draft.cursor = 0;
        for ch in "/switch ".chars() {
            ui.insert(&ch.to_string(), false);
        }
        assert!(
            enter(&mut ui).iter().any(|action| matches!(action, UiAction::SelectThread { thread_id, .. } if thread_id == "synthetic-thread"))
        );
    }
    #[test]
    fn pasted_name_or_name_suffix_is_never_authorized_by_a_typed_slash() {
        for (typed, pasted) in [
            ("", "/quit"),
            ("/", "quit"),
            ("/qu", "it"),
            ("/quit", "-suffix"),
        ] {
            let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
            ui.insert(typed, false);
            ui.insert(pasted, true);
            assert!(!ui.view().draft.command_is_typed());
            assert!(!ui.has_completions());
            assert!(matches!(
                enter(&mut ui).first(),
                Some(UiAction::Send { .. })
            ));
        }
    }
    #[test]
    fn deleting_pasted_prefix_does_not_promote_the_pasted_suffix() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("x/quit", true);
        ui.active_view_mut().draft.cursor = 0;
        ui.delete(false);
        assert!(!ui.view().draft.command_is_typed());
        for _ in 0..5 {
            ui.delete(false);
        }
        for ch in "/quit".chars() {
            ui.insert(&ch.to_string(), false);
        }
        assert!(ui.view().draft.command_is_typed());
        assert!(
            enter(&mut ui)
                .iter()
                .any(|action| matches!(action, UiAction::Quit))
        );
    }
    #[test]
    fn ordinary_text_has_no_transient_command_provenance() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("ordinary text", false);
        assert_eq!(ui.view().draft.typed_prefix, 0);
        let restored: DraftSnapshot =
            serde_json::from_value(serde_json::to_value(&ui.view().draft).unwrap()).unwrap();
        assert_eq!(restored, ui.view().draft);
    }
    #[test]
    fn provenance_is_transient_and_local_snapshots_preserve_it() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("/switch ", false);
        ui.insert("synthetic-thread", true);
        let local = ui.view().draft.clone();
        assert!(local.command_is_typed());
        let json = serde_json::to_value(&local).unwrap();
        assert!(json.get("typed_prefix").is_none());
        let restored: DraftSnapshot = serde_json::from_value(json).unwrap();
        assert!(!restored.command_is_typed());
    }
    #[test]
    fn long_paste_is_folded_and_never_submitted_on_insert() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert(&format!("/quit\n{}", "synthetic text ".repeat(100)), true);
        assert_eq!(ui.view().draft.folds.len(), 1);
        assert!(!ui.view().draft.folds[0].expanded);
        assert!(!ui.view().draft.command_is_typed());
        assert!(ui.take_deferred_actions().is_empty());
    }
    #[test]
    fn every_completion_has_help_and_dispatches_after_real_tab_completion() {
        let config = Config::default();
        let help = super::super::help::Page::guide(&config)
            .lines(80)
            .into_owned()
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
            .collect::<String>();
        let documented = help
            .split(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '/' | '-'))
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        for hint in COMMANDS {
            let words = hint.name.split_ascii_whitespace().collect::<Vec<_>>();
            assert!(
                documented
                    .windows(words.len())
                    .any(|tokens| tokens == words),
                "missing help: {}",
                hint.name
            );
            let mut ui = UiState::new(&config, std::env::temp_dir());
            ui.active_view_mut().draft.text = hint.name.into();
            ui.active_view_mut().draft.cursor = hint.name.len();
            assert_eq!(ui.completions()[0].name, hint.name);
            assert!(
                ui.composer_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                    .is_some_and(|actions| actions.is_empty())
            );
            assert_eq!(ui.view().draft.text, format!("{} ", hint.name));
            let argument = match hint.name {
                "/switch" | "/edit" | "/rewind" | "/fork" | "/copy" | "/detach" => "sample-id",
                "/effort" => "none",
                "/attach" => "sample.txt",
                "/search" => "needle",
                "/restore" => "0",
                "/paste" => "0 show",
                _ => "",
            };
            let text = format!("{}{argument}", ui.view().draft.text);
            ui.slash(&text);
            assert!(
                !ui.view().status.contains("未知のコマンド"),
                "{}",
                hint.name
            );
        }
    }
    #[test]
    fn agents_slash_displays_reference_and_selects_saved_or_new_thread() {
        let cwd = std::env::temp_dir();
        let mut form_ui = UiState::new(&Config::default(), cwd.clone());
        assert!(matches!(
            form_ui
                .execute_operation(Command::ThreadCreate(crate::protocol::ThreadCreateArgs {
                    agents: Some("role.md".into()),
                    cwd: None,
                    connection: None,
                    model: None,
                    effort: None,
                    parent_id: None,
                }))
                .as_slice(),
            [UiAction::SelectAgents { .. }]
        ));
        let mut ui = UiState::new(&Config::default(), cwd.clone());
        assert!(ui.slash("/agents").is_empty());
        assert!(matches!(ui.slash("/agents role.md").as_slice(),
            [UiAction::SelectAgents {args:a,..}] if a.agents.as_deref()==Some("role.md")));
        ui.active_view_mut().thread_id = Some("saved".into());
        ui.active_view_mut().agents_path = "original.md".into();
        assert!(matches!(ui.slash("/agents role.md").as_slice(),
            [UiAction::Command(Command::ThreadSet(a))] if a.thread_id.as_deref()==Some("saved") && a.agents.as_deref()==Some("role.md")));
        assert_eq!(
            ui.view().agents_path,
            "original.md",
            "wait for canonical backend response"
        );
        assert!(
            matches!(ui.slash("/cwd elsewhere").as_slice(),[UiAction::Command(Command::ThreadSet(a))] if a.agents.is_none())
        );
        assert_eq!(ui.view().agents_path, "original.md");
    }
    #[test]
    fn rewind_sends_the_picked_request_and_lets_the_owner_pick_the_path_node_before_it() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        // No saved Thread: an error, not an action.
        assert!(ui.slash("/rewind req").is_empty());
        ui.active_view_mut().thread_id = Some("saved".into());
        // An id that is not loaded is still sent: the owner resolves it.
        assert!(matches!(ui.slash("/rewind req").as_slice(),
            [UiAction::Command(Command::ThreadRewind(a))]
                if a.thread_id.as_deref() == Some("saved") && a.node_id == "req" && a.exclude_node));
        // No argument and no loaded request: nothing to pick.
        assert!(ui.slash("/rewind").is_empty());
    }
    #[test]
    fn agents_selection_reply_preserves_newer_text_and_rejects_stale_generation() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        let actions = ui.slash("/agents role.md");
        let [
            UiAction::SelectAgents {
                request_id,
                draft_id,
                generation,
                ..
            },
        ] = actions.as_slice()
        else {
            panic!("selection action");
        };
        let (request_id, draft_id, generation) =
            (request_id.clone(), draft_id.clone(), *generation);
        ui.insert("newer draft text", false);
        let details:ThreadDetails=serde_json::from_value(serde_json::json!({"agents_path":"/role.md","thread_id":"created","title":"","cwd":"/","connection":"fixture","model":"fixture","children_count":0,"shells_count":0})).unwrap();
        ui.apply(UiUpdate::AgentsSelected {
            request_id: request_id.clone(),
            draft_id: draft_id.clone(),
            generation: generation + 1,
            result: Ok(details.clone()),
        });
        assert!(ui.view().thread_id.is_none());
        assert_eq!(ui.view().draft.text, "newer draft text");
        ui.apply(UiUpdate::AgentsSelected {
            request_id: request_id.clone(),
            draft_id: draft_id.clone(),
            generation,
            result: Ok(details.clone()),
        });
        assert_eq!(ui.view().thread_id.as_deref(), Some("created"));
        assert_eq!(ui.view().agents_path, "/role.md");
        assert_eq!(ui.view().draft.text, "newer draft text");
        ui.take_deferred_actions();
        ui.apply(UiUpdate::AgentsSelected {
            request_id,
            draft_id,
            generation,
            result: Ok(details),
        });
        assert!(
            ui.take_deferred_actions().is_empty(),
            "duplicate result must not readopt"
        );
    }
    #[test]
    fn rejected_agents_admission_releases_only_matching_selection_and_restores_slash() {
        for closed in [false, true] {
            let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
            ui.insert("/agents role.md", false);
            let action = ui
                .event(crossterm::event::Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )))
                .remove(0);
            assert!(ui.view().selecting_agents.is_some());
            let (sender, receiver) = actions::channel(1);
            let rejected = if closed {
                drop(receiver);
                sender.try_send(action).unwrap_err().into_inner()
            } else {
                sender
                    .try_send(UiAction::ActivateDraft {
                        draft_id: "occupy".into(),
                    })
                    .unwrap();
                sender.try_send(action).unwrap_err().into_inner()
            };
            ui.action_rejected(&rejected);
            assert!(ui.view().selecting_agents.is_none());
            assert_eq!(
                ui.view().draft.text,
                "/agents role.md",
                "submitted slash must be restored"
            );
            let retry = ui
                .event(crossterm::event::Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )))
                .remove(0);
            let current = ui.view().selecting_agents.clone();
            assert!(current.is_some(), "retry must be available after rejection");
            ui.action_rejected(&rejected);
            assert_eq!(
                ui.view().selecting_agents,
                current,
                "stale rejection cannot release a new request"
            );
            ui.action_rejected(&retry);
            assert!(ui.view().selecting_agents.is_none());
        }
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("keep direct-form draft", false);
        let action = ui
            .execute_operation(Command::ThreadCreate(crate::protocol::ThreadCreateArgs {
                agents: Some("role.md".into()),
                cwd: None,
                connection: None,
                model: None,
                effort: None,
                parent_id: None,
            }))
            .remove(0);
        ui.action_rejected(&action);
        assert!(ui.view().selecting_agents.is_none());
        assert_eq!(ui.view().draft.text, "keep direct-form draft");
        assert!(
            ui.event(crossterm::event::Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )))
            .iter()
            .any(|a| matches!(a, UiAction::Send { .. })),
            "normal send must not remain blocked"
        );
    }
    #[cfg(unix)]
    #[test]
    fn non_utf8_thread_cwd_cannot_silently_change_slash_targets_or_drafts() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let invalid = std::env::temp_dir().join(OsString::from_vec(b"bad-\xff".to_vec()));
        for saved in [false, true] {
            for command in ["/cwd relative", "/attach relative"] {
                let mut ui = UiState::new(&Config::default(), invalid.clone());
                if saved {
                    ui.active_view_mut().thread_id = Some("saved-thread".into());
                }
                let draft = ui.view().draft.clone();
                assert!(ui.slash(command).is_empty());
                assert!(ui.view().status.contains("UTF-8"), "{}", ui.view().status);
                assert_eq!(ui.view().cwd, invalid);
                assert_eq!(ui.view().draft, draft);
            }
        }
        // An explicit representable absolute target remains usable from that cwd.
        let valid = std::env::temp_dir().join("representable-target");
        let mut ui = UiState::new(&Config::default(), invalid);
        assert!(ui.slash(&format!("/cwd {}", valid.display())).is_empty());
        assert_eq!(ui.view().cwd, valid);
    }
    #[test]
    fn thread_subcommands_complete_without_taking_over_other_filter_arguments() {
        for (query, expected) in [
            ("/threads", "/threads"),
            ("/threads a", "/threads active"),
            ("/threads d", "/threads descendants"),
        ] {
            let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
            for ch in query.chars() {
                ui.event(crossterm::event::Event::Key(KeyEvent::new(
                    KeyCode::Char(ch),
                    KeyModifiers::NONE,
                )));
            }
            assert_eq!(ui.completions()[0].name, expected);
            ui.composer_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
            assert_eq!(ui.view().draft.text, format!("{expected} "));
        }
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        for ch in "/threads project filter".chars() {
            ui.event(crossterm::event::Event::Key(KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::NONE,
            )));
        }
        assert!(ui.completions().is_empty());
    }
}
