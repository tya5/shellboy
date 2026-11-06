//! One-key rewind from the history view (#246): preview the owner's facts,
//! confirm, execute, then offer one key to open the removed branch.
//!
//! No I/O here. The facts come from `thread rewind --dry-run` (the owner runs
//! the real rewind inside a rolled-back savepoint), so the confirmation shows
//! exactly what the execution will report; nothing is estimated in the UI.
use super::*;
use serde_json::Value;

/// The facts a rewind reports, read from the owner's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RewindFacts {
    removed_nodes: i64,
    removed_turns: i64,
    shell_executions: i64,
    child_threads: i64,
    pending_replies: i64,
}

impl RewindFacts {
    /// `None` when the owner's result lacks a fact: never fill in a guess.
    pub(super) fn from_result(value: &Value) -> Option<Self> {
        let count = |pointer: &str| value.pointer(pointer).and_then(Value::as_i64);
        Some(Self {
            removed_nodes: count("/removed/nodes")?,
            removed_turns: count("/removed/turns")?,
            shell_executions: count("/shell_executions_after_node/count")?,
            child_threads: count("/child_threads_after_node/count")?,
            pending_replies: count("/pending_replies_to_sent_messages/count")?,
        })
    }

    pub(super) fn text(&self, language: crate::config::UiLanguage) -> String {
        use crate::config::UiLanguage;
        let none = |n: i64, unit: &str, zero: &str| {
            if n == 0 {
                zero.to_owned()
            } else {
                format!("{n}{unit}")
            }
        };
        match language {
            UiLanguage::Ja => format!(
                "この依頼の手前まで会話を戻しますか？\n\
                 \n\
                 外れる範囲: {} Turn、{} ノード\n\
                 戻らないもの:\n  \
                 この依頼より後のシェル実行: {}\n  \
                 ファイルの変更は戻りません\n  \
                 この依頼より後に始めた子の会話: {}\n  \
                 返事待ちの送信: {}\n\
                 取り消し: 外れた部分は枝の会話として残ります。戻した直後に {} で開けます（/branches からも開けます）",
                self.removed_turns,
                self.removed_nodes,
                none(self.shell_executions, "件", "なし"),
                none(self.child_threads, "件", "なし"),
                none(self.pending_replies, "件", "なし"),
                UNDO_BINDING,
            ),
            UiLanguage::En => format!(
                "Rewind this conversation to just before this request?\n\
                 \n\
                 Removed range: {} {}, {} {}\n\
                 Not undone:\n  \
                 Shell executions after this request: {}\n  \
                 File changes are not undone\n  \
                 Child conversations started after it: {}\n  \
                 Pending replies to sent messages: {}\n\
                 Undo: the removed part stays as a branch conversation. Right after rewinding, {} opens it (so does /branches)",
                self.removed_turns,
                plural(self.removed_turns, "turn", "turns"),
                self.removed_nodes,
                plural(self.removed_nodes, "node", "nodes"),
                none(self.shell_executions, "", "none"),
                none(self.child_threads, "", "none"),
                none(self.pending_replies, "", "none"),
                UNDO_BINDING,
            ),
        }
    }
}

fn plural<'a>(n: i64, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// The key that opens the branch right after a rewind (Alt chords already own
/// Alt+D / Alt+W while the composer is active; plain letters would be typed).
pub(super) const UNDO_BINDING: &str = "Alt+U";

/// What the one undo key opens: the branch holding the removed part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RewindUndo {
    pub thread_id: String,
    pub branch_id: String,
}

impl UiState {
    /// The key on a selected request in the history view.
    pub(super) fn rewind_selected(&mut self) -> Vec<UiAction> {
        let node = self.selected_history_item().and_then(|item| {
            let id = item.primary_node_id.clone();
            self.view()
                .nodes()
                .iter()
                .any(|node| node.node_id == id && node.role == "user")
                .then_some(id)
        });
        let (Some(thread_id), Some(node_id)) = (self.view().thread_id.clone(), node) else {
            return self.command_error(i18n::local!(
                self.config.ui.language,
                "巻き戻すには保存済みの依頼を選んでください",
                "Select a saved request to rewind to just before it"
            ));
        };
        self.begin_rewind(thread_id, node_id)
    }

    /// Ask the owner for the facts (`--dry-run`); the confirmation follows.
    pub(super) fn begin_rewind(&mut self, thread_id: String, node_id: String) -> Vec<UiAction> {
        if self.rewind_pending.is_some() {
            return vec![];
        }
        self.next_rewind_request += 1;
        let request_id = self.next_rewind_request;
        self.rewind_pending = Some(request_id);
        vec![UiAction::Rewind {
            request_id,
            args: ThreadRewindArgs {
                thread_id: Some(thread_id),
                node_id,
                exclude_node: true,
                dry_run: true,
            },
        }]
    }

    /// Confirmed with Y: the same request without `dry_run`.
    pub(super) fn execute_rewind(&mut self, mut args: ThreadRewindArgs) -> Vec<UiAction> {
        if self.rewind_pending.is_some() {
            return vec![];
        }
        args.dry_run = false;
        self.next_rewind_request += 1;
        let request_id = self.next_rewind_request;
        self.rewind_pending = Some(request_id);
        vec![UiAction::Rewind { request_id, args }]
    }

    pub(super) fn rewind_result(
        &mut self,
        request_id: u64,
        args: ThreadRewindArgs,
        result: Result<Value, String>,
    ) {
        if self.rewind_pending != Some(request_id) {
            return;
        }
        self.rewind_pending = None;
        let language = self.config.ui.language;
        let same_thread = self.view().thread_id == args.thread_id;
        let fail = |ui: &mut Self, message: String| {
            ui.active_view_mut().status = format!(
                "{}: {message}",
                i18n::local!(language, "巻き戻せません", "Cannot rewind")
            );
            ui.active_view_mut()
                .promote_status_notice(history_interaction::NoticeKind::Failure);
            ui.dirty = true;
        };
        match (args.dry_run, result) {
            (_, Err(message)) => fail(self, message),
            (true, Ok(value)) => {
                // The view moved on while the owner was reading: show nothing.
                if !same_thread {
                    return;
                }
                match RewindFacts::from_result(&value) {
                    Some(facts) => self.set_confirmation(Confirmation::Rewind { args, facts }),
                    None => fail(
                        self,
                        i18n::local!(
                            language,
                            "owner の結果に必要な項目がありません",
                            "the owner's result lacks required facts"
                        )
                        .into(),
                    ),
                }
            }
            (false, Ok(value)) => {
                let Some(branch_id) = value["branch_thread_id"].as_str() else {
                    return fail(
                        self,
                        i18n::local!(
                            language,
                            "owner の結果に枝の会話がありません",
                            "the owner's result names no branch conversation"
                        )
                        .into(),
                    );
                };
                let Some(thread_id) = args.thread_id.clone() else {
                    return;
                };
                self.rewind_undo = Some(RewindUndo {
                    thread_id,
                    branch_id: branch_id.to_owned(),
                });
                if same_thread {
                    self.active_view_mut().status = match language {
                        crate::config::UiLanguage::Ja => format!(
                            "依頼の手前まで戻しました。{UNDO_BINDING} で元に戻す（切り取った枝を開く）"
                        ),
                        crate::config::UiLanguage::En => format!(
                            "Rewound to before the request. {UNDO_BINDING} undoes it (opens the removed branch)"
                        ),
                    };
                    self.active_view_mut()
                        .promote_status_notice(history_interaction::NoticeKind::Info);
                }
                self.dirty = true;
            }
        }
    }

    /// Alt+U: open the branch Thread holding the removed part (one use).
    pub(super) fn undo_rewind(&mut self) -> Vec<UiAction> {
        let Some(undo) = self.rewind_undo.as_ref() else {
            return vec![];
        };
        if self.view().thread_id.as_deref() != Some(undo.thread_id.as_str()) {
            return vec![];
        }
        let branch = undo.branch_id.clone();
        self.rewind_undo = None;
        self.select_thread(branch)
    }

    pub(super) fn rewind_undo_available(&self) -> bool {
        self.rewind_undo
            .as_ref()
            .is_some_and(|undo| self.view().thread_id.as_deref() == Some(undo.thread_id.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UiLanguage;
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    fn facts_json(shells: i64, children: i64, replies: i64) -> Value {
        json!({
            "removed": {"nodes": 14, "turns": 2},
            "shell_executions_after_node": {"count": shells},
            "child_threads_after_node": {"count": children},
            "pending_replies_to_sent_messages": {"count": replies},
        })
    }
    fn fixture(language: UiLanguage) -> UiState {
        let mut config = Config::default();
        config.ui.language = language;
        let mut ui = UiState::new(&config, std::env::temp_dir());
        ui.select_thread("thread".into());
        let node = |id: &str, role: &str, text: &str| UiNode {
            node_id: id.into(),
            role: role.into(),
            text: text.into(),
            total_chars: text.chars().count(),
            ..Default::default()
        };
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            nodes: vec![
                node("req", "user", "please fix the tests"),
                node("ans", "assistant", "done"),
            ],
            pending: Default::default(),
            read_revision: 1,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 2,
        });
        Terminal::new(TestBackend::new(80, 30))
            .unwrap()
            .draw(|frame| draw(frame, &mut ui))
            .unwrap();
        ui
    }
    fn key(ui: &mut UiState, code: KeyCode) -> Vec<UiAction> {
        ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }
    fn rewind_action(ui: &mut UiState, ch: char) -> (u64, ThreadRewindArgs) {
        let actions = key(ui, KeyCode::Char(ch));
        let [UiAction::Rewind { request_id, args }] = actions.as_slice() else {
            panic!("expected one rewind action, got {actions:?}")
        };
        (*request_id, args.clone())
    }
    fn alt_u(ui: &mut UiState) -> Vec<UiAction> {
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::ALT,
        )))
    }
    fn screen(ui: &mut UiState, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 60)).unwrap();
        terminal.draw(|frame| draw(frame, ui)).unwrap();
        let buffer = terminal.backend().buffer();
        (buffer.area.y..buffer.area.bottom())
            .map(|y| {
                (buffer.area.x..buffer.area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// Select the user request in the history view.
    fn select_request(ui: &mut UiState) {
        key(ui, KeyCode::Tab);
        key(ui, KeyCode::Up);
        assert_eq!(
            ui.selected_history_item().unwrap().primary_node_id,
            "req",
            "fixture selection"
        );
    }

    #[test]
    fn the_rewind_key_conflicts_with_no_binding_in_the_history_focus() {
        use key_hints::{Action, Context};
        // Every action offered with a request selected has its own binding,
        // and none is one of the fixed navigation keys of that focus.
        let mut seen = std::collections::BTreeSet::new();
        for action in [Action::Rewind, Action::Copy, Action::Code, Action::Original] {
            assert!(
                seen.insert(action.spec().0),
                "duplicate binding {}",
                action.spec().0
            );
        }
        for fixed in ["Enter", "Tab", "Esc", "Space", "Enter/Space"] {
            assert!(!seen.contains(fixed));
        }
        assert_eq!(Action::Rewind.spec().0, "r");
        assert!(Action::Rewind.matches(KeyCode::Char('r')));
        // Only the request context offers it; other items do not.
        let mut ui = fixture(UiLanguage::En);
        key(&mut ui, KeyCode::Tab);
        assert_eq!(ui.hint_context(), Context::History);
        key(&mut ui, KeyCode::Up);
        assert_eq!(ui.hint_context(), Context::HistoryRequest);
        // The undo chord is not a plain letter, so composing is unaffected.
        assert_eq!(Action::Undo.spec().0, "Alt+U");
        assert!(!Action::Undo.matches(KeyCode::Char('u')));
        // Pressing every other history key on a request never starts a rewind.
        for code in [
            KeyCode::Char('c'),
            KeyCode::Char('b'),
            KeyCode::Char('o'),
            KeyCode::Char(' '),
        ] {
            select_request(&mut fixture(UiLanguage::En));
            let mut ui = fixture(UiLanguage::En);
            select_request(&mut ui);
            let actions = key(&mut ui, code);
            assert!(
                !actions.iter().any(|a| matches!(a, UiAction::Rewind { .. })),
                "{code:?}"
            );
        }
    }

    #[test]
    fn key_previews_confirms_executes_and_offers_the_undo() {
        for language in [UiLanguage::En, UiLanguage::Ja] {
            let mut ui = fixture(language);
            // On the answer the key starts nothing and says why.
            key(&mut ui, KeyCode::Tab);
            assert!(key(&mut ui, KeyCode::Char('r')).is_empty());
            assert!(ui.confirmation.is_none() && ui.rewind_pending.is_none());
            // On the request it asks the owner for the facts (dry run, before the request).
            key(&mut ui, KeyCode::Up);
            let (request_id, args) = rewind_action(&mut ui, 'r');
            assert_eq!(args.thread_id.as_deref(), Some("thread"));
            assert_eq!(args.node_id, "req");
            assert!(args.exclude_node && args.dry_run);
            // A second press while the owner is reading starts nothing.
            assert!(key(&mut ui, KeyCode::Char('r')).is_empty());
            // The facts arrive: a confirmation, nothing executed.
            ui.apply(UiUpdate::RewindResult {
                request_id,
                args: args.clone(),
                result: Ok(facts_json(3, 1, 0)),
            });
            assert!(matches!(ui.confirmation, Some(Confirmation::Rewind { .. })));
            // Cancel: no action, the confirmation is gone, a new attempt works.
            assert!(key(&mut ui, KeyCode::Char('n')).is_empty());
            assert!(ui.confirmation.is_none() && ui.rewind_pending.is_none());
            assert!(ui.rewind_undo.is_none());
            let (request_id, args) = rewind_action(&mut ui, 'r');
            ui.apply(UiUpdate::RewindResult {
                request_id,
                args,
                result: Ok(facts_json(0, 0, 0)),
            });
            // Esc also cancels.
            assert!(key(&mut ui, KeyCode::Esc).is_empty());
            assert!(ui.confirmation.is_none());
            // Y executes the same request for real.
            let (request_id, args) = rewind_action(&mut ui, 'r');
            ui.apply(UiUpdate::RewindResult {
                request_id,
                args,
                result: Ok(facts_json(3, 1, 0)),
            });
            let (request_id, args) = rewind_action(&mut ui, 'y');
            assert!(!args.dry_run && args.exclude_node && args.node_id == "req");
            // Before the result nothing is offered; Alt+U does nothing.
            assert!(alt_u(&mut ui).is_empty() && ui.rewind_undo.is_none());
            // The result names the branch: the undo is offered, in the footer notice.
            ui.apply(UiUpdate::RewindResult {
                request_id,
                args,
                result: Ok(json!({"branch_thread_id": "branch-1"})),
            });
            let shown = screen(&mut ui, 100);
            assert!(shown.contains("Alt+U"), "{shown}");
            // Alt+U opens the removed branch, once.
            let actions = alt_u(&mut ui);
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, UiAction::SelectThread { thread_id, .. } if thread_id == "branch-1"))
                    || ui.view().thread_id.as_deref() == Some("branch-1"),
                "{actions:?}"
            );
            assert!(ui.rewind_undo.is_none());
            assert!(alt_u(&mut ui).is_empty());
        }
    }

    #[test]
    fn owner_refusal_and_missing_facts_are_shown_not_guessed() {
        let mut ui = fixture(UiLanguage::En);
        select_request(&mut ui);
        let (request_id, args) = rewind_action(&mut ui, 'r');
        ui.apply(UiUpdate::RewindResult {
            request_id,
            args,
            result: Err("Thread has a live Run".into()),
        });
        assert!(ui.confirmation.is_none() && ui.rewind_pending.is_none());
        assert!(screen(&mut ui, 100).contains("Cannot rewind: Thread has a live Run"));
        // A result lacking a fact never becomes a confirmation.
        let (request_id, args) = rewind_action(&mut ui, 'r');
        ui.apply(UiUpdate::RewindResult {
            request_id,
            args,
            result: Ok(json!({"removed": {"nodes": 1, "turns": 1}})),
        });
        assert!(ui.confirmation.is_none());
        // A stale result (unknown request id) is ignored.
        ui.apply(UiUpdate::RewindResult {
            request_id: 999,
            args: ThreadRewindArgs {
                thread_id: Some("thread".into()),
                node_id: "req".into(),
                exclude_node: true,
                dry_run: true,
            },
            result: Ok(facts_json(0, 0, 0)),
        });
        assert!(ui.confirmation.is_none());
    }

    #[test]
    fn slash_rewind_goes_through_the_same_preview() {
        let mut ui = fixture(UiLanguage::En);
        assert!(matches!(
            ui.slash("/rewind req").as_slice(),
            [UiAction::Rewind { args, .. }] if args.dry_run && args.exclude_node && args.node_id == "req"
        ));
    }

    #[test]
    fn confirmation_shows_the_owners_facts_in_both_languages_and_narrow_widths() {
        // Wrapping splits phrases across box rows: compare the body text only.
        let squash = |text: &str| {
            text.lines()
                .map(|line| line.trim().trim_matches(['│', '|']))
                .collect::<String>()
                .replace(['\u{a0}', ' '], "")
        };
        for (language, expected) in [
            (
                UiLanguage::En,
                vec![
                    "Rewind this conversation to just before this request?",
                    "Removed range: 2 turns, 14 nodes",
                    "Shell executions after this request: 3",
                    "File changes are not undone",
                    "Child conversations started after it: 1",
                    "Pending replies to sent messages: none",
                    "Undo: the removed part stays as a branch conversation",
                    "Alt+U",
                    "Y Execute",
                ],
            ),
            (
                UiLanguage::Ja,
                vec![
                    "この依頼の手前まで会話を戻しますか？",
                    "外れる範囲: 2 Turn、14 ノード",
                    "この依頼より後のシェル実行: 3件",
                    "ファイルの変更は戻りません",
                    "この依頼より後に始めた子の会話: 1件",
                    "返事待ちの送信: なし",
                    "取り消し: 外れた部分は枝の会話として残ります",
                    "Alt+U",
                    "Y 実行",
                ],
            ),
        ] {
            for observed in [Some(1), Some(2), None] {
                crate::ui::text_width::set_terminal_width(observed);
                for width in [100u16, 60, 40, 28] {
                    let mut ui = fixture(language);
                    select_request(&mut ui);
                    let (request_id, args) = rewind_action(&mut ui, 'r');
                    ui.apply(UiUpdate::RewindResult {
                        request_id,
                        args,
                        result: Ok(facts_json(3, 1, 0)),
                    });
                    let shown = screen(&mut ui, width);
                    if observed.is_none() && width == 100 || observed == Some(1) && width == 28 {
                        println!("--- {language:?} {observed:?} {width} columns ---\n{shown}");
                    }
                    let flat = squash(&shown);
                    for phrase in &expected {
                        // The shared confirmation footer is cut by the frame below ~40
                        // columns (existing behaviour for every confirmation).
                        if width < 40 && phrase.starts_with("Y ") {
                            continue;
                        }
                        assert!(
                            flat.contains(&squash(phrase)),
                            "{language:?} {observed:?} width {width}: missing {phrase:?}\n{shown}"
                        );
                    }
                }
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }

    #[test]
    fn plural_and_zero_counts_read_naturally() {
        let one = RewindFacts {
            removed_nodes: 1,
            removed_turns: 1,
            shell_executions: 0,
            child_threads: 0,
            pending_replies: 0,
        };
        let text = one.text(UiLanguage::En);
        assert!(text.contains("1 turn, 1 node"), "{text}");
        assert!(text.contains("Shell executions after this request: none"));
    }
}
