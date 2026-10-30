//! Completion is local; prompt recall is a bounded asynchronous DB read.
use super::theme;
use super::*;
use std::collections::VecDeque;

struct CommandHint {
    name: &'static str,
    help: &'static str,
}
const COMMANDS: &[CommandHint] = &[
    CommandHint {
        name: "/agents",
        help: "指示ファイル参照を確認／変更",
    },
    CommandHint {
        name: "/received",
        help: "受信記録（失敗・ストリーム差分）を読む",
    },
    CommandHint {
        name: "/status",
        help: "作業状態・背景コマンド・子の作業を確認",
    },
    CommandHint {
        name: "/help",
        help: "操作ガイド",
    },
    CommandHint {
        name: "/new",
        help: "新しい会話",
    },
    CommandHint {
        name: "/threads",
        help: "会話一覧",
    },
    CommandHint {
        name: "/switch",
        help: "IDで会話を切替",
    },
    CommandHint {
        name: "/model",
        help: "接続・モデルを確認／変更",
    },
    CommandHint {
        name: "/models",
        help: "接続先のモデル一覧・選択",
    },
    CommandHint {
        name: "/effort",
        help: "推論強度を変更",
    },
    CommandHint {
        name: "/cwd",
        help: "作業ディレクトリを確認／変更",
    },
    CommandHint {
        name: "/attach",
        help: "ファイルを添付",
    },
    CommandHint {
        name: "/attachments",
        help: "添付一覧",
    },
    CommandHint {
        name: "/detach",
        help: "添付を外す",
    },
    CommandHint {
        name: "/reload-attachments",
        help: "添付を再読込",
    },
    CommandHint {
        name: "/image",
        help: "画像を貼付け",
    },
    CommandHint {
        name: "/paste-image",
        help: "画像を貼付け（別名）",
    },
    CommandHint {
        name: "/search",
        help: "履歴を検索",
    },
    CommandHint {
        name: "/index",
        help: "検索索引を管理",
    },
    CommandHint {
        name: "/fork",
        help: "履歴から会話を分岐",
    },
    CommandHint {
        name: "/edit",
        help: "依頼を編集して分岐",
    },
    CommandHint {
        name: "/compact-max",
        help: "APIなしで最大縮小",
    },
    CommandHint {
        name: "/compact",
        help: "会話を圧縮",
    },
    CommandHint {
        name: "/details",
        help: "履歴の詳細・原文",
    },
    CommandHint {
        name: "/links",
        help: "完全URL一覧",
    },
    CommandHint {
        name: "/actions",
        help: "操作フォーム",
    },
    CommandHint {
        name: "/operations",
        help: "操作フォーム（別名）",
    },
    CommandHint {
        name: "/copy",
        help: "原文・コードをコピー",
    },
    CommandHint {
        name: "/reasoning",
        help: "推論の表示を切替",
    },
    CommandHint {
        name: "/select",
        help: "端末の選択モード",
    },
    CommandHint {
        name: "/paste",
        help: "貼付けblockの確認・編集",
    },
    CommandHint {
        name: "/restore",
        help: "入力範囲を戻す。番号付きは失敗した下書きを復元",
    },
    CommandHint {
        name: "/retry",
        help: "現在の復旧待ちを同じ内容で再試行",
    },
    CommandHint {
        name: "/stop",
        help: "現在の会話の作業を停止",
    },
    CommandHint {
        name: "/cancel-wait",
        help: "待機の観測を取消",
    },
    CommandHint {
        name: "/cancel",
        help: "実行を中断",
    },
    CommandHint {
        name: "/background",
        help: "foreground shellを背景化",
    },
    CommandHint {
        name: "/quit",
        help: "通常終了",
    },
    CommandHint {
        name: "/queue",
        help: "保存済みの反映待ち・保留入力を確認／取消",
    },
];

#[derive(Debug, Default)]
pub(super) struct Composer {
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
            view.status = "入力できます".into();
        }
    }
    pub(super) fn has_completions(&self) -> bool {
        !self.completions().is_empty()
    }
    fn completions(&self) -> Vec<&'static CommandHint> {
        let draft = &self.view().draft;
        if draft.literal_paste
            || draft.cursor != draft.text.len()
            || !draft.text.starts_with('/')
            || draft.text.chars().any(char::is_whitespace)
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
                        self.active_view_mut().status =
                            "補完結果が下書きのbyte上限を超えます".into();
                        return Some(vec![]);
                    }
                    self.begin_edit();
                    let draft = &mut self.active_view_mut().draft;
                    draft.text = completed;
                    draft.cursor = draft.text.len();
                    draft.folds.clear();
                    draft.literal_paste = false;
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
        view.status = "入力履歴を読込み中".into();
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
            view.status = "編集中の下書きに戻りました".into();
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
            view.status =
                "入力履歴の操作待ちが上限です。読み込み後にもう一度操作してください".into();
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
                    self.active_view_mut().status =
                        "入力履歴が下書き上限を超えます。本文は変更していません".into();
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
                view.draft.revision = view.draft.revision.wrapping_add(1);
                let recall = view.composer.recall.as_mut().unwrap();
                recall.ordinal = Some(ordinal);
                recall.node_id = Some(node_id);
                recall.shown_revision = view.draft.revision;
                view.status = "入力履歴 · ↑古い / ↓新しい・下書きへ".into();
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
                view.status = "これより古い入力はありません".into();
            }
            Err(error) => {
                recall.queued.clear();
                recall.ordinal = None;
                recall.node_id = None;
                view.status = format!("入力履歴エラー: {error}");
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
                    hint.help
                ))
                .style(
                    Style::default()
                        .fg(if active { theme::ACCENT } else { theme::TEXT })
                        .bg(theme::DEFAULT_BACKGROUND),
                )
            })
            .collect::<Vec<_>>();
        lines.push(
            Line::from("↑↓ 選択 · Tab/Enter 補完 · Esc 閉じる")
                .style(Style::default().fg(theme::MUTED)),
        );
        frame.render_widget(Paragraph::new(lines), popup);
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;
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
            .collect::<std::collections::BTreeSet<_>>();
        for hint in COMMANDS {
            assert!(
                documented.contains(hint.name),
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
                "/switch" | "/edit" | "/fork" | "/copy" | "/detach" => "sample-id",
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
}
