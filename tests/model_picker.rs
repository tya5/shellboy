use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use shellboy::{
    config::{Config, Connection},
    protocol::Command,
    ui::*,
};
use unicode_width::UnicodeWidthStr;
fn config() -> Config {
    let mut config = Config {
        default_connection: Some("proxy".into()),
        default_model: Some("model-b".into()),
        ..Default::default()
    };
    config.connections.insert(
        "proxy".into(),
        Connection {
            base_url: "http://127.0.0.1:4000/v1".into(),
            ..Default::default()
        },
    );
    config
}
fn state(saved: bool) -> UiState {
    let mut state = UiState::new(&config(), std::env::temp_dir());
    if saved {
        state.select_thread("model-thread".into());
    }
    state
}
fn key(state: &mut UiState, code: KeyCode) -> Vec<UiAction> {
    state.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}
fn open(state: &mut UiState, command: &str) -> UiAction {
    state.insert(command, false);
    key(state, KeyCode::Enter).remove(0)
}
fn response(state: &mut UiState, action: &UiAction, result: Result<Vec<&str>, &str>) {
    let UiAction::LoadModels { request_id, .. } = action else {
        panic!("{action:?}")
    };
    state.apply(UiUpdate::Models {
        request_id: *request_id,
        result: result
            .map(|ids| ids.into_iter().map(String::from).collect())
            .map_err(String::from),
    });
}
fn screen(state: &mut UiState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
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
#[test]
fn live_list_marks_current_filters_and_changes_only_the_current_saved_thread() {
    let mut state = state(true);
    let request = open(&mut state, "/models");
    assert!(matches!(&request,UiAction::LoadModels{connection,..}if connection=="proxy"));
    assert!(screen(&mut state, 100, 30).contains("取得中"));
    response(
        &mut state,
        &request,
        Ok(vec!["model-a", "model-b", "model-c"]),
    );
    let shown = screen(&mut state, 100, 30);
    assert!(shown.contains("model-b  ● 現在"));
    assert!(!shown.contains("\"data\""));
    key(&mut state, KeyCode::Char('a'));
    let shown = screen(&mut state, 100, 30);
    assert!(shown.contains("model-a"));
    assert!(!shown.contains("model-c"));
    let action = key(&mut state, KeyCode::Enter).remove(0);
    assert!(
        matches!(action,UiAction::Command(Command::ThreadSet(args))if args.thread_id.as_deref()==Some("model-thread")&&args.connection.as_deref()==Some("proxy")&&args.model.as_deref()==Some("model-a")&&args.effort.is_none())
    );
    assert_eq!(
        state.view().model.as_deref(),
        Some("model-b"),
        "backend confirmation owns saved state"
    );
}
#[test]
fn unsaved_selection_keeps_effort_and_does_not_change_new_thread_defaults() {
    let mut state = state(false);
    state.view_mut().effort = Some("low".into());
    let request = open(&mut state, "/models proxy");
    response(&mut state, &request, Ok(vec!["model-a", "model-b"]));
    key(&mut state, KeyCode::Up);
    assert!(key(&mut state, KeyCode::Enter).is_empty());
    assert_eq!(state.view().model.as_deref(), Some("model-a"));
    assert_eq!(state.view().effort.as_deref(), Some("low"));
    state.new_chat();
    assert_eq!(state.view().model.as_deref(), Some("model-b"));
}
#[test]
fn loading_filter_is_preserved_and_escape_refresh_reject_late_results() {
    let mut state = state(true);
    let old = open(&mut state, "/models");
    key(&mut state, KeyCode::Esc);
    response(&mut state, &old, Ok(vec!["stale-model"]));
    assert!(!screen(&mut state, 100, 30).contains("stale-model"));
    let request = open(&mut state, "/models");
    key(&mut state, KeyCode::Char('a'));
    response(&mut state, &old, Ok(vec!["stale-model"]));
    response(&mut state, &request, Ok(vec!["model-a", "model-b"]));
    assert!(
        matches!(&key(&mut state,KeyCode::Enter)[0],UiAction::Command(Command::ThreadSet(args))if args.model.as_deref()==Some("model-a"))
    );
    let request = open(&mut state, "/models");
    response(&mut state, &request, Err("HTTP 404 models unavailable"));
    assert!(screen(&mut state, 100, 30).contains("HTTP 404"));
    let refresh = state
        .event(Event::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        )))
        .remove(0);
    response(&mut state, &request, Ok(vec!["stale-model"]));
    assert!(screen(&mut state, 100, 30).contains("取得中"));
    response(&mut state, &refresh, Ok(vec![]));
    assert!(screen(&mut state, 100, 30).contains("一覧は空"));
    assert!(key(&mut state, KeyCode::Enter).is_empty());
}
#[test]
fn modal_paste_and_wheel_do_not_edit_or_scroll_the_background_and_full_id_can_be_copied() {
    let mut state = state(true);
    let request = open(&mut state, "/models");
    state.insert("background draft", false);
    response(
        &mut state,
        &request,
        Ok(vec!["long-model-with-complete-id"]),
    );
    state.event(Event::Paste("long\n".into()));
    assert_eq!(state.view().draft.text, "background draft");
    let follow = state.view().follow;
    state.event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(state.view().follow, follow);
    let action = state
        .event(Event::Key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::CONTROL,
        )))
        .remove(0);
    assert!(matches!(action,UiAction::CopyText{text,..}if text=="long-model-with-complete-id"));
    for size in [(8, 5), (20, 10), (100, 30)] {
        screen(&mut state, size.0, size.1);
    }
}
#[test]
fn queue_rejection_and_unknown_connection_are_visible_and_switch_closes_the_picker() {
    let mut state = state(true);
    state.insert("/models unknown", false);
    assert!(key(&mut state, KeyCode::Enter).is_empty());
    assert!(state.view().status.contains("コマンドエラー"));
    state.view_mut().draft = DraftSnapshot::default();
    let request = open(&mut state, "/models");
    state.action_rejected(&request);
    assert!(screen(&mut state, 100, 30).contains("キュー"));
    state.select_thread("another-thread".into());
    response(&mut state, &request, Ok(vec!["stale-model"]));
    assert!(!screen(&mut state, 100, 30).contains("stale-model"));
}

#[test]
fn shared_list_keeps_last_wrapped_model_selected_and_footer_fixed() {
    let mut state = state(true);
    let request = open(&mut state, "/models");
    let mut models = (0..39).map(|i| format!("model-{i:02}")).collect::<Vec<_>>();
    let last = format!("last-model-{}{}", "a".repeat(100), "日本語🙂".repeat(18));
    models.push(last.clone());
    response(
        &mut state,
        &request,
        Ok(models.iter().map(String::as_str).collect()),
    );
    for _ in 0..39 {
        key(&mut state, KeyCode::Down);
    }
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let shown = screen(&mut state, 80, 24);
    assert!(shown.contains("last-model-"));
    assert!(shown.contains("Esc 戻る") && shown.contains("Ctrl+Y コピー"));
    assert!(shown.contains("40 / 40件"));
    let buffer = terminal.backend().buffer();
    let selected_y = (0..24)
        .find(|y| (0..80).any(|x| buffer[(x, *y)].symbol() == "›"))
        .unwrap();
    assert_eq!(
        buffer[(40, selected_y)].bg,
        ratatui::style::Color::Rgb(32, 44, 58)
    );
    let copied = state.event(Event::Key(KeyEvent::new(
        KeyCode::Char('y'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(&copied[..], [UiAction::CopyText { text, .. }] if text == &last));
}

#[test]
fn oversized_model_stays_visible_without_truncating_the_copy_target() {
    let mut state = state(true);
    let request = open(&mut state, "/models");
    let model = format!("oversize-{}-TAIL", "日本語🙂".repeat(300));
    response(&mut state, &request, Ok(vec![&model]));
    for (width, height) in [(80, 24), (20, 10)] {
        let shown = screen(&mut state, width, height);
        assert!(
            shown.contains("oversize-"),
            "selected item at {width}x{height}"
        );
        assert!(shown.contains('…') && shown.contains("Esc"));
    }
    let copied = state.event(Event::Key(KeyEvent::new(
        KeyCode::Char('y'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(&copied[..], [UiAction::CopyText { text, .. }] if text == &model));
}

#[test]
fn model_page_uses_resize_before_drawing_and_never_reuses_an_old_list_height() {
    for old_draw in [false, true] {
        let mut state = state(true);
        let request = open(&mut state, "/models");
        let models: Vec<_> = (0..100).map(|index| format!("m{index:03}")).collect();
        response(
            &mut state,
            &request,
            Ok(models.iter().map(String::as_str).collect()),
        );
        if old_draw {
            screen(&mut state, 120, 45);
        }
        state.event(Event::Resize(80, 20));
        key(&mut state, KeyCode::PageDown);
        let actions = key(&mut state, KeyCode::Enter);
        assert!(
            matches!(&actions[..],[UiAction::Command(Command::ThreadSet(args))] if args.model.as_deref() == Some("m006")),
            "{actions:?}"
        );
    }
}

#[test]
fn narrow_picker_preserves_the_meaning_of_primary_actions_in_fixed_footer() {
    let mut state = state(true);
    let request = open(&mut state, "/models");
    response(&mut state, &request, Ok(vec!["model-a", "model-b"]));
    let shown = screen(&mut state, 35, 18);
    assert!(shown.contains("Esc 戻る"), "{shown}");
    assert!(shown.contains("Enter 変更"), "{shown}");
}
