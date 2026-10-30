use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use shellboy::{config::Config, ui::*};
use unicode_width::UnicodeWidthStr;

fn state() -> UiState {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("composer-thread".into());
    ui
}
fn key(ui: &mut UiState, code: KeyCode) -> Vec<UiAction> {
    ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}
fn type_text(ui: &mut UiState, text: &str) {
    for ch in text.chars() {
        key(ui, KeyCode::Char(ch));
    }
}
fn screen(ui: &mut UiState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, ui)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            let mut line = String::new();
            let mut x = 0;
            while x < width {
                let symbol = buffer[(x, y)].symbol();
                line.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn answer(ui: &mut UiState, action: &UiAction, item: Option<(u64, &str)>) {
    let UiAction::RecallInput {
        thread_id,
        generation,
        request_id,
        ..
    } = action
    else {
        panic!("not a recall: {action:?}");
    };
    ui.apply(UiUpdate::RecalledInput {
        thread_id: thread_id.clone(),
        generation: *generation,
        request_id: *request_id,
        result: Ok(item.map(|(ordinal, text)| RecalledPrompt {
            ordinal,
            node_id: format!("source-{ordinal}"),
            text: text.into(),
        })),
    });
}

#[test]
fn suggestions_draw_filter_select_and_complete_without_executing() {
    let mut ui = state();
    type_text(&mut ui, "/at");
    let shown = screen(&mut ui, 100, 30);
    assert!(shown.contains("/attach"));
    assert!(shown.contains("/attachments"));
    assert!(!shown.contains("/compact"));
    assert!(key(&mut ui, KeyCode::Down).is_empty());
    assert!(key(&mut ui, KeyCode::Tab).is_empty());
    assert_eq!(ui.view().draft.text, "/attachments ");
    assert!(ui.management.is_none());
    assert!(!screen(&mut ui, 100, 30).contains("Tab/Enter 補完"));
}

#[test]
fn restore_completion_explains_the_context_and_draft_argument_distinction() {
    let mut ui = state();
    type_text(&mut ui, "/rest");
    let shown = screen(&mut ui, 120, 30);
    assert!(shown.contains("/restore"));
    assert!(shown.contains("入力範囲"));
    assert!(shown.contains("下書き"));
    assert!(key(&mut ui, KeyCode::Tab).is_empty());
    assert_eq!(ui.view().draft.text, "/restore ");
}
#[test]
fn completion_and_recalled_text_respect_the_current_draft_byte_limit() {
    let mut config = Config::default();
    config.limits.draft_bytes = 4;
    let mut ui = UiState::new(&config, std::env::temp_dir());
    ui.select_thread("composer-thread".into());
    type_text(&mut ui, "/he");
    key(&mut ui, KeyCode::Tab);
    assert_eq!(ui.view().draft.text, "/he");
    assert!(ui.view().status.contains("上限"));
    key(&mut ui, KeyCode::Esc);
    let request = key(&mut ui, KeyCode::Up).remove(0);
    answer(&mut ui, &request, Some((20, "longer than the new limit")));
    assert_eq!(ui.view().draft.text, "/he");
    assert!(ui.view().status.contains("上限"));
}
#[test]
fn enter_completes_a_prefix_and_exact_commands_keep_their_original_behavior() {
    let mut ui = state();
    type_text(&mut ui, "/he");
    assert!(key(&mut ui, KeyCode::Enter).is_empty());
    assert_eq!(ui.view().draft.text, "/help ");
    assert!(!ui.help_visible);
    key(&mut ui, KeyCode::Enter);
    assert!(ui.help_visible);
    let mut ui = state();
    type_text(&mut ui, "/help");
    key(&mut ui, KeyCode::Enter);
    assert!(ui.help_visible);
}
#[test]
fn escape_hides_the_menu_until_edit_and_pasted_slashes_are_literal() {
    let mut ui = state();
    type_text(&mut ui, "/he");
    key(&mut ui, KeyCode::Esc);
    assert_eq!(ui.view().draft.text, "/he");
    assert!(!screen(&mut ui, 80, 24).contains("Tab/Enter 補完"));
    key(&mut ui, KeyCode::Backspace);
    type_text(&mut ui, "e");
    assert!(screen(&mut ui, 80, 24).contains("Tab/Enter 補完"));
    let mut ui = state();
    ui.insert("/help", true);
    assert!(!screen(&mut ui, 80, 24).contains("Tab/Enter 補完"));
    assert!(
        matches!(&key(&mut ui, KeyCode::Enter)[0], UiAction::Send { text, .. } if text=="/help")
    );
    assert!(!ui.help_visible);
}
#[test]
fn popup_fits_small_terminals_and_scrolls_to_the_selected_candidate() {
    let mut ui = state();
    type_text(&mut ui, "/");
    key(&mut ui, KeyCode::Up); // /queue is the final candidate.
    key(&mut ui, KeyCode::Up); // Select /quit above it.
    assert!(screen(&mut ui, 90, 24).contains("/quit"));
    for (width, height) in [(8, 5), (20, 10), (35, 20)] {
        screen(&mut ui, width, height);
    }
    key(&mut ui, KeyCode::Tab);
    assert_eq!(ui.view().draft.text, "/quit ");
    assert!(!ui.is_quitting());
}
#[test]
fn recall_restores_original_draft_cursor_folds_attachments_and_literal_flag() {
    let mut ui = state();
    ui.insert("作成中\n貼付け", true);
    ui.view_mut().draft.cursor = 3;
    ui.view_mut().draft.attachments.push(UiAttachment {
        id: "attachment".into(),
        path: "/synthetic".into(),
        kind: "file".into(),
        state: "ready".into(),
        position: None,
    });
    ui.view_mut().draft.folds.push(PasteFold {
        id: 4,
        start: 0,
        end: 3,
        expanded: false,
    });
    let original = ui.view().draft.clone();
    let actions = key(&mut ui, KeyCode::Up);
    answer(&mut ui, &actions[0], Some((20, "前の依頼\n二行目🙂")));
    assert_eq!(ui.view().draft.text, "前の依頼\n二行目🙂");
    assert_eq!(ui.view().draft.attachments, original.attachments);
    let actions = key(&mut ui, KeyCode::Down);
    answer(&mut ui, &actions[0], None);
    let mut restored = ui.view().draft.clone();
    assert!(restored.revision > original.revision);
    restored.revision = original.revision;
    assert_eq!(restored, original);
}
#[test]
fn multiline_arrows_edit_inside_the_draft_and_recall_at_the_boundary() {
    let mut ui = state();
    ui.insert("first\nsecond\nthird", false);
    assert!(key(&mut ui, KeyCode::Up).is_empty());
    assert_eq!(ui.view().draft.cursor, 11);
    assert!(key(&mut ui, KeyCode::Up).is_empty());
    assert_eq!(ui.view().draft.cursor, 5);
    assert!(matches!(
        &key(&mut ui, KeyCode::Up)[0],
        UiAction::RecallInput {
            before_ordinal: None,
            older: true,
            ..
        }
    ));
}
#[test]
fn rapid_arrows_replay_each_unconsumed_history_step() {
    let mut ui = state();
    let first = key(&mut ui, KeyCode::Up).remove(0);
    assert!(key(&mut ui, KeyCode::Up).is_empty());
    assert!(key(&mut ui, KeyCode::Up).is_empty());
    answer(&mut ui, &first, Some((30, "newest")));
    let second = ui.take_deferred_actions().pop().unwrap();
    assert!(matches!(
        second,
        UiAction::RecallInput {
            before_ordinal: Some(30),
            older: true,
            ..
        }
    ));
    answer(&mut ui, &second, Some((20, "middle")));
    let third = ui.take_deferred_actions().pop().unwrap();
    answer(&mut ui, &third, Some((10, "oldest")));
    assert!(ui.take_deferred_actions().is_empty());
    assert_eq!(ui.view().draft.text, "oldest");
    let down = key(&mut ui, KeyCode::Down).remove(0);
    assert!(matches!(
        down,
        UiAction::RecallInput {
            before_ordinal: Some(10),
            older: false,
            ..
        }
    ));
}
#[test]
fn mixed_arrow_bursts_preserve_order_at_oldest_and_draft_boundaries() {
    // Compare asynchronous bursts against an ordinary clamped shell history.
    for bits in 0..64 {
        let mut ui = state();
        ui.insert("original draft", false);
        let mut requests = std::collections::VecDeque::new();
        let mut expected: usize = 2;
        for step in 0..6 {
            let older = bits & (1 << step) != 0;
            expected = if older {
                expected.saturating_sub(1)
            } else {
                (expected + 1).min(2)
            };
            requests.extend(key(
                &mut ui,
                if older { KeyCode::Up } else { KeyCode::Down },
            ));
        }
        let mut reads = 0;
        while let Some(request) = requests.pop_front() {
            reads += 1;
            assert!(reads < 20);
            let UiAction::RecallInput {
                before_ordinal,
                older,
                ..
            } = &request
            else {
                panic!("{request:?}");
            };
            let item = if *older {
                [(10, "oldest"), (20, "newest")]
                    .into_iter()
                    .rev()
                    .find(|(ordinal, _)| before_ordinal.is_none_or(|before| *ordinal < before))
            } else {
                [(10, "oldest"), (20, "newest")]
                    .into_iter()
                    .find(|(ordinal, _)| before_ordinal.is_some_and(|before| *ordinal > before))
            };
            answer(&mut ui, &request, item);
            requests.extend(ui.take_deferred_actions());
        }
        assert_eq!(
            ui.view().draft.text,
            ["oldest", "newest", "original draft"][expected],
            "sequence={bits:06b}"
        );
    }
}
#[test]
fn up_then_enter_waits_for_the_recalled_text_and_editing_cancels_a_queued_send() {
    let mut ui = state();
    ui.insert("unfinished", false);
    let request = key(&mut ui, KeyCode::Up).remove(0);
    assert!(key(&mut ui, KeyCode::Enter).is_empty());
    answer(&mut ui, &request, Some((20, "requested history")));
    assert!(
        matches!(&ui.take_deferred_actions()[0], UiAction::Send { text, .. } if text=="requested history")
    );

    let mut ui = state();
    ui.insert("unfinished", false);
    let request = key(&mut ui, KeyCode::Up).remove(0);
    key(&mut ui, KeyCode::Enter);
    ui.insert(" edited", false);
    answer(&mut ui, &request, Some((20, "stale history")));
    assert!(ui.take_deferred_actions().is_empty());
    assert_eq!(ui.view().draft.text, "unfinished edited");
}
#[test]
fn editing_and_switching_reject_late_replies_without_sticking_navigation() {
    let mut ui = state();
    let old = key(&mut ui, KeyCode::Up).remove(0);
    ui.insert("new draft", false);
    answer(&mut ui, &old, Some((9, "stale")));
    assert_eq!(ui.view().draft.text, "new draft");
    assert!(!ui.view().status.contains("読込み中"));
    let old = key(&mut ui, KeyCode::Up).remove(0);
    ui.select_thread("other".into());
    answer(&mut ui, &old, Some((9, "stale")));
    assert!(ui.view().draft.text.is_empty());
    ui.select_thread("composer-thread".into());
    assert!(matches!(
        &key(&mut ui, KeyCode::Up)[0],
        UiAction::RecallInput { .. }
    ));
}
#[test]
fn recalled_slash_is_sent_as_message_and_read_errors_leave_the_draft_intact() {
    let mut ui = state();
    let action = key(&mut ui, KeyCode::Up).remove(0);
    answer(&mut ui, &action, Some((5, "/quit")));
    assert!(
        matches!(&key(&mut ui, KeyCode::Enter)[0], UiAction::Send { text, .. } if text=="/quit")
    );
    assert!(!ui.is_quitting());
    let mut ui = state();
    ui.insert("unfinished", false);
    let action = key(&mut ui, KeyCode::Up).remove(0);
    ui.action_rejected(&action);
    assert_eq!(ui.view().draft.text, "unfinished");
    assert!(ui.view().status.contains("キュー"));
    assert!(matches!(
        &key(&mut ui, KeyCode::Up)[0],
        UiAction::RecallInput { .. }
    ));
}

#[test]
fn candidate_catalog_matches_every_dispatched_slash_name() {
    // Public behavior: each exact slash name from the dispatcher appears in
    // the completion catalog; adding a command cannot silently omit its hint.
    let source = include_str!("../src/ui.rs");
    let dispatcher = source
        .split("fn slash(")
        .nth(1)
        .unwrap()
        .split("fn ")
        .next()
        .unwrap();
    let mut names = std::collections::BTreeSet::new();
    for token in dispatcher.split('"').skip(1).step_by(2) {
        if token.starts_with('/') && !token.chars().any(char::is_whitespace) {
            names.insert(token);
        }
    }
    let catalog = include_str!("../src/ui/composer.rs");
    for name in names {
        assert!(
            catalog.contains(&format!("name: \"{name}\"")),
            "Missing {name}"
        );
    }
}
