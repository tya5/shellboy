use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use shellboy::{
    config::{Config, Connection},
    ui::*,
};
use std::path::PathBuf;
use unicode_width::UnicodeWidthStr;
fn screen_text(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let area = buffer.area;
    let mut out = String::new();
    for y in area.y..area.bottom() {
        let mut x = area.x;
        while x < area.right() {
            let symbol = buffer[(x, y)].symbol();
            out.push_str(symbol);
            x += symbol.width().max(1) as u16;
        }
        out.push('\n');
    }
    out
}
fn command_action(action: &UiAction) -> &UiAction {
    match action {
        UiAction::SubmittedCommand { action, .. } | UiAction::ManagementIntent { action, .. } => {
            command_action(action)
        }
        action => action,
    }
}
fn command_actions(actions: &[UiAction]) -> Vec<UiAction> {
    actions
        .iter()
        .map(|action| command_action(action).clone())
        .collect()
}

#[test]
fn argumentless_restore_edits_only_the_current_saved_threads_input_range() {
    use shellboy::protocol::{Command, ContextEditAction, ContextEditArgs};
    let mut ui = state();
    ui.select_thread("current-thread".into());
    ui.insert("/restore", false);
    let actions = command_actions(&enter(&mut ui));
    assert_eq!(actions.len(), 1);
    assert!(
        matches!(&actions[0], UiAction::Command(Command::ContextEdit(ContextEditArgs {
        thread_id: Some(thread), action: ContextEditAction::Restore,
        from_node_id: None, to_node_id: None,
    })) if thread == "current-thread")
    );

    let mut unsaved = state();
    unsaved.insert("/restore", false);
    assert!(enter(&mut unsaved).is_empty());
    assert!(unsaved.view().status.contains("保存済み会話"));

    let mut invalid = state();
    invalid.select_thread("current-thread".into());
    invalid.insert("/restore not-a-draft-number", false);
    assert!(enter(&mut invalid).is_empty());
    assert!(invalid.view().status.contains("引数なし"));
    assert!(invalid.view().status.contains("下書き"));
}

fn cwd() -> PathBuf {
    std::env::temp_dir()
}
fn state() -> UiState {
    UiState::new(&Config::default(), cwd())
}
fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn enter(ui: &mut UiState) -> Vec<UiAction> {
    ui.event(key(KeyCode::Enter))
}
fn text(ui: &mut UiState, value: &str) {
    for ch in value.chars() {
        ui.event(key(KeyCode::Char(ch)));
    }
}
fn sent(action: &[UiAction]) -> (String, u64) {
    match &action[0] {
        UiAction::Send {
            draft_id, revision, ..
        } => (draft_id.clone(), *revision),
        _ => panic!("expected Send"),
    }
}
#[test]
fn acceptance_clears_only_its_captured_draft() {
    let mut ui = state();
    text(&mut ui, "first");
    let actions = enter(&mut ui);
    let (draft_id, revision) = sent(&actions);
    assert_eq!(ui.view().draft.text, "first");
    text(&mut ui, "next");
    assert_eq!(ui.view().draft.text, "firstnext");
    ui.apply(UiUpdate::Receipt {
        draft_id,
        revision,
        thread_id: Some("thread-1".into()),
        status: ReceiptStatus::Accepted,
        message: None,
    });
    assert_eq!(ui.view().draft.text, "firstnext");
    assert!(
        ui.view()
            .status
            .contains("編集中の入力を保持（次は全文送信）")
    );
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread-1"));
    let actions = enter(&mut ui);
    let (draft_id, revision) = sent(&actions);
    ui.apply(UiUpdate::Receipt {
        draft_id,
        revision,
        thread_id: None,
        status: ReceiptStatus::Accepted,
        message: None,
    });
    assert!(ui.view().draft.text.is_empty());
    assert!(!ui.view().status.contains("次は全文送信"));
}
#[test]
fn queue_rejection_and_save_failure_keep_text_and_snapshot() {
    let mut ui = state();
    text(&mut ui, "retain me");
    let actions = enter(&mut ui);
    let (_, old_revision) = sent(&actions);
    ui.action_rejected(&actions[0]);
    assert_eq!(ui.view().draft.text, "retain me");
    assert_eq!(
        ui.view().pending[&old_revision].status,
        ReceiptStatus::Failed
    );
    let retried = enter(&mut ui);
    let (draft_id, new_revision) = sent(&retried);
    assert!(new_revision > old_revision);
    ui.apply(UiUpdate::Receipt {
        draft_id,
        revision: old_revision,
        thread_id: None,
        status: ReceiptStatus::Accepted,
        message: None,
    });
    assert_eq!(ui.view().draft.revision, new_revision);
    assert_eq!(ui.view().draft.text, "retain me");
}
#[test]
fn paste_never_executes_or_sends_and_preserves_unicode_newlines() {
    let mut ui = state();
    assert!(
        ui.event(Event::Paste("/quit\r\n  日本語😀\r\n".into()))
            .is_empty()
    );
    assert!(!ui.is_quitting());
    assert_eq!(ui.view().draft.text, "/quit\n  日本語😀\n");
    let actions = enter(&mut ui);
    assert!(matches!(&actions[0], UiAction::Send { text, .. } if text == "/quit\n  日本語😀\n"));
}
#[test]
fn long_paste_folds_inspects_edits_removes_and_sends_full_text() {
    let mut ui = state();
    let pasted = "long 日本語 line\n".repeat(100);
    ui.event(Event::Paste(pasted.clone()));
    assert_eq!(ui.view().draft.folds.len(), 1);
    let (display, _) = draft_display(&ui.view().draft);
    assert!(display.contains("貼付け1"));
    assert!(!display.contains("long 日本語"));
    ui.event(key(KeyCode::F(3)));
    assert!(ui.view().draft.folds[0].expanded);
    assert!(draft_display(&ui.view().draft).0.contains("long 日本語"));
    ui.event(key(KeyCode::F(4)));
    assert_eq!(ui.view().draft.cursor, 0);
    ui.event(key(KeyCode::End));
    let actions = enter(&mut ui);
    assert!(matches!(&actions[0], UiAction::Send { text, .. } if text == &pasted));
    let mut ui = state();
    ui.event(Event::Paste(pasted));
    ui.event(key(KeyCode::F(5)));
    assert!(ui.view().draft.text.is_empty());
    assert!(ui.view().draft.folds.is_empty());
}
#[test]
fn grapheme_editing_and_cursor_cells_handle_japanese_emoji_combining() {
    let mut ui = state();
    ui.insert("日👨‍👩‍👧‍👦e\u{301}", false);
    let end = ui.view().draft.text.len();
    ui.event(key(KeyCode::Left));
    assert_eq!(ui.view().draft.cursor, end - 3);
    ui.event(key(KeyCode::Backspace));
    assert_eq!(ui.view().draft.text, "日e\u{301}");
    assert_eq!(ui.view().draft.cursor, "日".len());
    assert_eq!(
        cursor_position("日e\u{301}😀", "日e\u{301}".len(), 20),
        (3, 0)
    );
    assert_eq!(cursor_position("日本", "日本".len(), 3), (2, 1));
}
#[test]
fn thread_switch_restores_draft_and_anchor_and_rejects_stale_pages() {
    let mut ui = state();
    ui.select_thread("one".into());
    let generation = ui.view().generation;
    text(&mut ui, "draft one");
    ui.view_mut().anchor = ViewAnchor {
        node_id: Some("old-node".into()),
        char_offset: 17,
    };
    ui.view_mut().follow = false;
    let actions = ui.select_thread("two".into());
    assert!(matches!(&actions[0], UiAction::SaveDraft { thread_id, .. } if thread_id == "one"));
    text(&mut ui, "draft two");
    ui.apply(UiUpdate::History {
        thread_id: "two".into(),
        generation,
        nodes: vec![UiNode {
            presentation: Default::default(),
            node_id: "stale".into(),
            role: "assistant".into(),
            text: "wrong".into(),
            base_char_offset: 0,
            total_chars: 5,
            reasoning_ranges: vec![],
        }],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 0,
    });
    assert!(ui.view().nodes().is_empty());
    ui.select_thread("one".into());
    assert_eq!(ui.view().draft.text, "draft one");
    assert_eq!(ui.view().anchor.char_offset, 17);
    assert!(!ui.view().follow);
}
#[test]
fn selection_mode_keeps_follow_and_allows_keyboard_release_without_interrupting() {
    let mut ui = state();
    ui.select_thread("one".into());
    let generation = ui.view().generation;
    assert!(ui.event(key(KeyCode::F(2))).is_empty());
    assert!(ui.selection_mode);
    ui.apply(UiUpdate::Stream {
        thread_id: "one".into(),
        generation,
        node_id: "new".into(),
        role: "assistant".into(),
        text: "background continues".into(),
        base_char_offset: 0,
        total_chars: 20,
        reasoning_ranges: vec![],
        presentation: Default::default(),
        ack: None,
    });
    assert_eq!(ui.view().nodes().len(), 1);
    assert!(ui.view().follow);
    assert!(ui.event(key(KeyCode::Esc)).is_empty());
    assert!(!ui.selection_mode);
    assert!(ui.view().follow);
}
#[test]
fn configurable_keys_and_interrupt_background_quit_are_explicit_actions() {
    let mut config = Config::default();
    config.ui.send_key = "Ctrl+Enter".into();
    config.ui.newline_keys = vec!["Enter".into()];
    let mut ui = UiState::new(&config, cwd());
    text(&mut ui, "one");
    assert!(enter(&mut ui).is_empty());
    assert_eq!(ui.view().draft.text, "one\n");
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(actions[0], UiAction::Send { .. }));
    ui.view_mut().thread_id = Some("thread".into());
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(actions[0], UiAction::Cancel { .. }));
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('b'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(actions[0], UiAction::Background { .. }));
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(actions.last(), Some(UiAction::Quit)));
    assert!(ui.is_quitting());
}
#[test]
fn commands_on_new_draft_preserve_configuration_and_resolve_paths_without_io() {
    let mut config = Config::default();
    config.connections.insert(
        "fixture".into(),
        Connection {
            base_url: "https://example.com/v1".into(),
            ..Connection::default()
        },
    );
    let mut ui = UiState::new(&config, cwd());
    text(&mut ui, "/model fixture arbitrary-model");
    assert!(enter(&mut ui).is_empty());
    assert_eq!(ui.view().model.as_deref(), Some("arbitrary-model"));
    assert!(ui.view().draft.text.is_empty());
    text(&mut ui, "/effort arbitrary-effort");
    enter(&mut ui);
    assert_eq!(ui.view().effort.as_deref(), Some("arbitrary-effort"));
    text(&mut ui, "/attach 'missing file.txt'");
    enter(&mut ui);
    assert_eq!(
        ui.view().draft.attachments[0].path,
        cwd().join("missing file.txt")
    );
    assert_eq!(ui.view().draft.text, "\u{fffc}");
    assert_eq!(ui.view().draft.attachments[0].position, Some(0));
    text(&mut ui, "hello");
    let actions = enter(&mut ui);
    assert!(
        matches!(&actions[0], UiAction::Send { model, effort, attachments, .. } if model.as_deref()==Some("arbitrary-model") && effort.as_deref()==Some("arbitrary-effort") && attachments.len()==1)
    );
}
#[test]
fn clipboard_result_is_bound_to_draft_revision_and_does_not_follow_switch() {
    let mut ui = state();
    ui.select_thread("one".into());
    let draft_id = ui.view().draft.draft_id.clone();
    let revision = ui.view().draft.revision;
    ui.select_thread("two".into());
    ui.apply(UiUpdate::ClipboardAttachment {
        draft_id: draft_id.clone(),
        revision,
        attachment: UiAttachment {
            id: "img".into(),
            path: cwd().join("fixture.png"),
            kind: "image".into(),
            state: "ready".into(),
            position: None,
        },
    });
    assert!(ui.view().draft.attachments.is_empty());
    ui.select_thread("one".into());
    assert_eq!(ui.view().draft.attachments.len(), 1);
    text(&mut ui, "edit");
    ui.apply(UiUpdate::ClipboardAttachment {
        draft_id,
        revision,
        attachment: UiAttachment {
            id: "late".into(),
            path: cwd().join("late.png"),
            kind: "image".into(),
            state: "ready".into(),
            position: None,
        },
    });
    assert_eq!(ui.view().draft.attachments.len(), 1);
}
#[test]
fn test_backend_renders_status_modes_and_native_unicode_cursor() {
    let mut ui = state();
    ui.insert("日本語😀", false);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let rendered = screen_text(&terminal);
    assert!(rendered.replace(' ', "").starts_with("[入力できます]"));
    assert!(rendered.replace(' ', "").contains("追従中"));
    assert!(rendered.replace(' ', "").contains("日本語"));
    ui.event(key(KeyCode::F(2)));
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let rendered = screen_text(&terminal);
    assert!(
        rendered.replace(' ', "").contains("表示固定") && rendered.contains("F2/Esc解除"),
        "{rendered}"
    );
}
#[test]
fn draft_byte_limit_rejects_whole_paste_and_keeps_original() {
    let mut config = Config::default();
    config.limits.draft_bytes = 8;
    let mut ui = UiState::new(&config, cwd());
    text(&mut ui, "old");
    ui.event(Event::Paste("日本語".into()));
    assert_eq!(ui.view().draft.text, "old");
    assert!(ui.view().status.contains("上限"));
}

#[test]
fn new_chat_keeps_previous_unsaved_draft_accessible_in_thread_list() {
    let mut ui = state();
    text(&mut ui, "unsaved original");
    let original = ui.active.clone();
    ui.new_chat();
    text(&mut ui, "/threads");
    enter(&mut ui);
    let local = ui
        .threads()
        .iter()
        .find(|thread| thread.thread_id == format!("draft:{original}"))
        .unwrap()
        .thread_id
        .clone();
    ui.select_thread(local);
    assert_eq!(ui.view().draft.text, "unsaved original");
}

#[test]
fn control_c_delivered_in_selection_mode_retains_interrupt_semantics() {
    let mut ui = state();
    ui.select_thread("thread".into());
    ui.event(key(KeyCode::F(2)));
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(actions.first(),Some(UiAction::Cancel{thread_id}) if thread_id=="thread"));
    assert!(ui.selection_mode);
}

#[test]
fn restored_view_is_ignored_after_user_navigation_and_osc52_is_deferred() {
    let mut ui = state();
    let actions = ui.select_thread("one".into());
    let (generation, view_revision) = actions
        .iter()
        .find_map(|action| match command_action(action) {
            UiAction::SelectThread {
                generation,
                view_revision,
                ..
            } => Some((*generation, *view_revision)),
            _ => None,
        })
        .unwrap();
    ui.apply(UiUpdate::ViewState {
        thread_id: "one".into(),
        generation,
        view_revision,
        anchor: ViewAnchor {
            node_id: Some("original".into()),
            char_offset: 120_000,
        },
        follow: false,
    });
    assert_eq!(ui.view().anchor.char_offset, 120_000);
    ui.event(Event::Key(KeyEvent::new(
        KeyCode::End,
        KeyModifiers::CONTROL,
    )));
    ui.apply(UiUpdate::ViewState {
        thread_id: "one".into(),
        generation,
        view_revision,
        anchor: ViewAnchor {
            node_id: Some("late".into()),
            char_offset: 1,
        },
        follow: false,
    });
    assert!(ui.view().follow);
    ui.apply(UiUpdate::TerminalRequest {
        bytes: b"fixture escape".to_vec(),
    });
    assert_eq!(
        ui.take_terminal_requests(),
        vec![b"fixture escape".to_vec()]
    );
    assert!(ui.take_terminal_requests().is_empty());
}

#[test]
fn original_character_anchor_requests_window_outside_cached_tail() {
    let mut ui = state();
    ui.select_thread("one".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::History {
        thread_id: "one".into(),
        generation,
        nodes: vec![UiNode {
            presentation: Default::default(),
            node_id: "large".into(),
            role: "assistant".into(),
            text: "tail".into(),
            base_char_offset: 99_996,
            total_chars: 100_000,
            reasoning_ranges: vec![],
        }],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 0,
    });
    ui.apply(UiUpdate::ViewState {
        thread_id: "one".into(),
        generation,
        view_revision: ui.view().view_revision,
        anchor: ViewAnchor {
            node_id: Some("large".into()),
            char_offset: 50_000,
        },
        follow: false,
    });
    let actions = ui.event(key(KeyCode::PageUp));
    assert!(actions.iter().any(|action| matches!(action, UiAction::LoadNodeWindow { node_id, char_offset, .. } if node_id == "large" && *char_offset == 50_000)));
    assert!(!ui.view().follow);
}

#[test]
fn edit_request_stages_separate_draft_and_requires_specific_run_stop_consent() {
    let mut ui = state();
    ui.select_thread("old-thread".into());
    text(&mut ui, "existing unsent draft 日本語");
    let old_key = ui.active.clone();
    let old = ui.view().draft.clone();
    let generation = ui.view().generation;
    ui.apply(UiUpdate::EditRequestLoaded {
        thread_id: "old-thread".into(),
        node_id: "original-user".into(),
        generation,
        draft_revision: old.revision,
        text: "original request 日本語".into(),
        attachments: vec![],
        run_id: Some("old-run".into()),
    });
    assert_ne!(ui.active, old_key);
    assert_eq!(ui.views[&old_key].draft, old);
    assert_eq!(
        ui.view().draft.edit.as_ref().unwrap().thread_id,
        "old-thread"
    );
    ui.insert(" edited", false);
    assert!(enter(&mut ui).is_empty());
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(screen_text(&terminal).contains("old-run"));
    assert!(ui.event(key(KeyCode::Char('n'))).is_empty());
    assert!(ui.view().pending.is_empty());
    assert!(enter(&mut ui).is_empty());
    let actions = ui.event(key(KeyCode::Char('y')));
    let (draft_id, revision) = match &actions[0] {
        UiAction::SendEdited {
            draft_id,
            revision,
            source_thread_id,
            source_node_id,
            confirmed_run_id,
            text,
            ..
        } => {
            assert_eq!(source_thread_id, "old-thread");
            assert_eq!(source_node_id, "original-user");
            assert_eq!(confirmed_run_id.as_deref(), Some("old-run"));
            assert_eq!(text, "original request 日本語 edited");
            (draft_id.clone(), *revision)
        }
        _ => panic!("expected edited send"),
    };
    ui.apply(UiUpdate::Receipt {
        draft_id,
        revision,
        thread_id: Some("fork-thread".into()),
        status: ReceiptStatus::Accepted,
        message: None,
    });
    assert_eq!(ui.view().thread_id.as_deref(), Some("fork-thread"));
    assert!(ui.view().draft.edit.is_none());
    assert_eq!(ui.views[&old_key].draft, old);
}

#[test]
fn stale_original_load_and_new_run_confirmation_preserve_revision_boundaries() {
    let mut ui = state();
    ui.select_thread("old".into());
    let generation = ui.view().generation;
    let revision = ui.view().draft.revision;
    text(&mut ui, "new typing");
    ui.apply(UiUpdate::EditRequestLoaded {
        thread_id: "old".into(),
        node_id: "user".into(),
        generation,
        draft_revision: revision,
        text: "stale".into(),
        attachments: vec![],
        run_id: None,
    });
    assert_eq!(ui.view().draft.text, "new typing");
    let revision = ui.view().draft.revision;
    ui.apply(UiUpdate::EditRequestLoaded {
        thread_id: "old".into(),
        node_id: "user".into(),
        generation,
        draft_revision: revision,
        text: "saved original".into(),
        attachments: vec![],
        run_id: None,
    });
    let actions = enter(&mut ui);
    let (draft_id, revision) = match &actions[0] {
        UiAction::SendEdited {
            draft_id,
            revision,
            confirmed_run_id: None,
            ..
        } => (draft_id.clone(), *revision),
        _ => panic!("expected send without stop"),
    };
    ui.apply(UiUpdate::EditConfirmationRequired {
        draft_id,
        revision,
        run_id: "new-run".into(),
    });
    let actions = ui.event(key(KeyCode::Char('y')));
    assert!(
        matches!(&actions[0],UiAction::SendEdited{revision:new_revision,confirmed_run_id:Some(id),..}if *new_revision>revision && id=="new-run")
    );
}

#[test]
fn index_overlay_requires_explicit_scope_and_cost_confirmation_and_shows_results() {
    use shellboy::protocol::{Command, IndexScope};
    let mut ui = state();
    text(&mut ui, "/index");
    let actions = enter(&mut ui);
    assert!(
        matches!(command_action(&actions[0]),UiAction::Manage(Command::IndexStatus(args))if args.scope.is_none())
    );
    assert!(ui.event(key(KeyCode::Char('b'))).is_empty());
    assert!(ui.event(key(KeyCode::Char('y'))).is_empty());
    ui.event(key(KeyCode::Esc));
    text(&mut ui, "/index all");
    let actions = enter(&mut ui);
    assert!(actions.iter().any(
        |action| matches!(command_action(action),UiAction::IndexPreview(args)if args.scope==Some(IndexScope::All))
    ));
    ui.apply(UiUpdate::Management {
        title: "index status all".into(),
        text: "pending=3 running=1 done=4 failed=2 paused=1".into(),
    });
    ui.apply(UiUpdate::Management {
        title: "preview all".into(),
        text: "sources=10 segments=20 characters=1000 already_indexed=4 unavailable=1".into(),
    });
    assert!(ui.management.as_ref().unwrap().text.contains("pending=3"));
    assert!(ui.management.as_ref().unwrap().text.contains("sources=10"));
    assert!(ui.event(key(KeyCode::Char('b'))).is_empty());
    assert!(ui.event(key(KeyCode::Char('y'))).is_empty());
    let args = actions
        .iter()
        .find_map(|action| match command_action(action) {
            UiAction::IndexPreview(args) => Some(args.clone()),
            _ => None,
        })
        .unwrap();
    ui.apply(UiUpdate::IndexPreviewReady {
        args,
        text: "sources=10 segments=20 characters=1000 already_indexed=4 unavailable=1".into(),
    });
    ui.event(key(KeyCode::Char('b')));
    assert!(enter(&mut ui).is_empty());
    let actions = ui.event(key(KeyCode::Char('y')));
    assert!(
        matches!(command_action(&actions[0]),UiAction::Manage(Command::IndexBackfill(args))if args.scope==Some(IndexScope::All))
    );
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let shown = screen_text(&terminal);
    assert!(shown.contains("pending=3"));
    assert!(shown.contains("sources=10"));
    ui.event(key(KeyCode::Esc));
    text(&mut ui, "/search query 日本語");
    let actions = enter(&mut ui);
    assert!(
        matches!(command_action(&actions[0]),UiAction::Manage(Command::HistorySearch(args))if args.query=="query 日本語")
    );
    ui.apply(UiUpdate::Management {
        title: "Search results".into(),
        text: "thread=selected node=result snippet=answer".into(),
    });
    assert!(ui.management.as_ref().unwrap().text.contains("node=result"));
}

#[test]
fn force_exit_is_unavailable_before_grace_and_requires_second_explicit_choice() {
    let mut ui = state();
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(actions.last(), Some(UiAction::Quit)));
    assert!(ui.event(key(KeyCode::Char('f'))).is_empty());
    assert!(ui.event(key(KeyCode::Char('y'))).is_empty());
    ui.apply(UiUpdate::ShutdownProgress {
        elapsed_ms: 5000,
        detail: "Run停止・保存待ち".into(),
        force_available: true,
    });
    assert!(matches!(
        &ui.event(key(KeyCode::Char('w')))[0],
        UiAction::WaitForShutdown
    ));
    assert!(ui.event(key(KeyCode::Char('f'))).is_empty());
    assert!(enter(&mut ui).is_empty());
    assert!(matches!(
        &ui.event(key(KeyCode::Char('y')))[0],
        UiAction::ForceQuit
    ));
    ui.apply(UiUpdate::ForceShutdown);
    assert!(ui.is_stopped());
    assert!(ui.is_forced());
}

#[test]
fn unsaved_model_change_preserves_explicit_effort() {
    let mut config = Config::default();
    config
        .connections
        .insert("profile".into(), Connection::default());
    let mut ui = UiState::new(&config, cwd());
    text(&mut ui, "/effort high");
    enter(&mut ui);
    text(&mut ui, "/model profile different-model");
    enter(&mut ui);
    assert_eq!(ui.view().effort.as_deref(), Some("high"));
}

#[test]
fn space_status_is_readonly_until_scope_and_matching_preview_are_present() {
    use shellboy::protocol::{Command, IndexArgs, IndexScope};
    let mut ui = state();
    text(&mut ui, "/index space old-space");
    let actions = enter(&mut ui);
    assert_eq!(actions.len(), 1);
    assert!(
        matches!(command_action(&actions[0]),UiAction::Manage(Command::IndexStatus(args))if args.space_id.as_deref()==Some("old-space") && args.scope.is_none())
    );
    assert!(ui.event(key(KeyCode::Char('t'))).is_empty());
    assert!(ui.event(key(KeyCode::Char('y'))).is_empty());
    ui.event(key(KeyCode::Esc));
    text(&mut ui, "/index space selected-space threads one two");
    let actions = enter(&mut ui);
    let selected = actions
        .iter()
        .find_map(|action| match command_action(action) {
            UiAction::IndexPreview(args) => Some(args.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(selected.space_id.as_deref(), Some("selected-space"));
    assert_eq!(selected.thread_ids, vec!["one", "two"]);
    ui.apply(UiUpdate::IndexPreviewReady {
        args: IndexArgs {
            space_id: Some("wrong-space".into()),
            ..selected.clone()
        },
        text: "wrong preview".into(),
    });
    ui.event(key(KeyCode::Char('r')));
    assert!(ui.event(key(KeyCode::Char('y'))).is_empty());
    ui.apply(UiUpdate::IndexPreviewReady {
        args: selected,
        text: "sources=2 segments=4 characters=500".into(),
    });
    ui.event(key(KeyCode::Char('r')));
    let actions = ui.event(key(KeyCode::Char('y')));
    assert!(
        matches!(command_action(&actions[0]),UiAction::Manage(Command::IndexRebuild(args))if args.scope==Some(IndexScope::Threads) && args.space_id.as_deref()==Some("selected-space"))
    );
    ui.event(key(KeyCode::Char('r')));
    assert!(ui.event(key(KeyCode::Char('y'))).is_empty());
}

#[test]
fn quit_leaves_selection_mode_and_remains_available_during_confirmation() {
    let mut ui = state();
    ui.event(key(KeyCode::F(2)));
    assert!(ui.selection_mode);
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL,
    )));
    assert!(ui.is_quitting());
    assert!(!ui.selection_mode);
    assert!(matches!(actions.last(), Some(UiAction::Quit)));
    let mut ui = state();
    ui.select_thread("source".into());
    ui.apply(UiUpdate::EditRequestLoaded {
        thread_id: "source".into(),
        node_id: "user".into(),
        generation: ui.view().generation,
        draft_revision: ui.view().draft.revision,
        text: "request".into(),
        attachments: vec![],
        run_id: Some("run".into()),
    });
    assert!(enter(&mut ui).is_empty());
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(actions.last(), Some(UiAction::Quit)));
}

#[test]
fn local_effort_clear_is_sent_as_explicit_empty_value_and_survives_model_change() {
    let mut config = Config {
        default_connection: Some("profile".into()),
        ..Config::default()
    };
    config.connections.insert(
        "profile".into(),
        Connection {
            default_effort: Some("high".into()),
            ..Connection::default()
        },
    );
    let mut ui = UiState::new(&config, cwd());
    assert_eq!(ui.view().effort.as_deref(), Some("high"));
    text(&mut ui, "/effort none");
    enter(&mut ui);
    text(&mut ui, "/model profile another");
    enter(&mut ui);
    text(&mut ui, "request");
    let actions = enter(&mut ui);
    assert!(matches!(&actions[0],UiAction::Send{effort:Some(effort),..}if effort.is_empty()));
    let mut ui = UiState::new(&config, cwd());
    text(&mut ui, "/effort none");
    enter(&mut ui);
    ui.view_mut().draft.edit = Some(EditRequestTarget {
        thread_id: "source".into(),
        node_id: "node".into(),
        run_id: None,
    });
    text(&mut ui, "edited request");
    let actions = enter(&mut ui);
    assert!(matches!(&actions[0],UiAction::SendEdited{effort:Some(effort),..}if effort.is_empty()));
}

#[test]
fn saved_attachment_management_lists_reloads_and_detaches_without_touching_draft_files() {
    use shellboy::protocol::Command;
    let mut ui = state();
    ui.select_thread("saved".into());
    text(&mut ui, "/attachments");
    let actions = enter(&mut ui);
    assert!(
        matches!(command_action(&actions[0]),UiAction::Manage(Command::AttachmentList(args))if args.thread_id.as_deref()==Some("saved"))
    );
    let actions = ui.event(key(KeyCode::Char('r')));
    assert!(
        matches!(command_action(&actions[0]),UiAction::ReloadAttachments{thread_id}if thread_id=="saved")
    );
    ui.apply(UiUpdate::Management {
        title: "Attachment selection".into(),
        text: "persisted-id selected=false managed=false unavailable_reason=excluded".into(),
    });
    assert!(
        ui.management
            .as_ref()
            .unwrap()
            .text
            .contains("selected=false")
    );
    ui.event(key(KeyCode::Esc));
    text(&mut ui, "/detach persisted-id");
    let actions = enter(&mut ui);
    assert!(
        matches!(command_action(&actions[0]),UiAction::Command(Command::AttachmentRemove(args))if args.attachment_id=="persisted-id" && args.thread_id.as_deref()==Some("saved"))
    );
    text(&mut ui, "/reload-attachments");
    let actions = enter(&mut ui);
    assert!(
        matches!(command_action(&actions[0]),UiAction::ReloadAttachments{thread_id}if thread_id=="saved")
    );
}

#[test]
fn reasoning_is_folded_by_default_and_toggle_preserves_node_and_source_offsets() {
    let mut ui = state();
    ui.select_thread("t".into());
    let generation = ui.view().generation;
    let original = "公開前文\n秘密の推論日本語\n公開回答";
    let start = "公開前文\n".chars().count();
    let end = start + "秘密の推論日本語".chars().count();
    ui.apply(UiUpdate::History {
        thread_id: "t".into(),
        generation,
        nodes: vec![UiNode {
            presentation: Default::default(),
            node_id: "canonical-node".into(),
            role: "assistant".into(),
            text: original.into(),
            base_char_offset: 0,
            total_chars: original.chars().count(),
            reasoning_ranges: vec![(start, end)],
        }],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 0,
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let shown = screen_text(&terminal);
    assert!(!shown.contains("秘密の推論"));
    assert!(shown.contains("/reasoning"));
    assert!(!shown.contains("canonical-node"));
    assert!(shown.contains("公開回答"));
    assert_eq!(ui.view().nodes()[0].text, original);
    text(&mut ui, "/reasoning canonical-node");
    assert!(enter(&mut ui).is_empty());
    assert_eq!(
        ui.view().anchor,
        ViewAnchor {
            node_id: Some("canonical-node".into()),
            char_offset: start
        }
    );
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(screen_text(&terminal).contains("秘密の推論日本語"));
    text(&mut ui, "/copy canonical-node");
    let actions = enter(&mut ui);
    assert!(
        matches!(command_action(&actions[0]),UiAction::Copy{node_id,..}if node_id=="canonical-node")
    );
    text(&mut ui, "/reasoning canonical-node");
    enter(&mut ui);
    assert!(!ui.view().reasoning_expanded.contains("canonical-node"));
    assert_eq!(ui.view().anchor.char_offset, start);
}

#[test]
fn reasoning_ranges_use_original_unicode_offsets_inside_a_loaded_window() {
    let mut ui = state();
    ui.select_thread("t".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::History {
        thread_id: "t".into(),
        generation,
        nodes: vec![UiNode {
            presentation: Default::default(),
            node_id: "large-node".into(),
            role: "assistant".into(),
            text: "推論日本語\n回答".into(),
            base_char_offset: 100,
            total_chars: 108,
            reasoning_ranges: vec![(50, 105)],
        }],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 0,
    });
    ui.apply(UiUpdate::ViewState {
        thread_id: "t".into(),
        generation,
        view_revision: ui.view().view_revision,
        anchor: ViewAnchor {
            node_id: Some("large-node".into()),
            char_offset: 102,
        },
        follow: false,
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(!screen_text(&terminal).contains("推論日本語"));
    assert_eq!(ui.view().anchor.char_offset, 102);
    text(&mut ui, "/reasoning large-node");
    let actions = enter(&mut ui);
    assert!(
        matches!(&actions[0],UiAction::LoadNodeWindow{node_id,char_offset:50,..}if node_id=="large-node")
    );
    assert_eq!(ui.view().anchor.char_offset, 50);
}

#[test]
fn a_new_stream_tail_keeps_reasoning_folded_in_the_anchored_old_window() {
    let mut ui = state();
    ui.select_thread("t".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::Stream {
        thread_id: "t".into(),
        generation,
        node_id: "canonical".into(),
        role: "assistant".into(),
        text: "推論秘密文\n回答".into(),
        base_char_offset: 0,
        total_chars: 8,
        reasoning_ranges: vec![(0, 5)],
        presentation: Default::default(),
        ack: None,
    });
    ui.apply(UiUpdate::ViewState {
        thread_id: "t".into(),
        generation,
        view_revision: ui.view().view_revision,
        anchor: ViewAnchor {
            node_id: Some("canonical".into()),
            char_offset: 2,
        },
        follow: false,
    });
    ui.apply(UiUpdate::Stream {
        thread_id: "t".into(),
        generation,
        node_id: "canonical".into(),
        role: "assistant".into(),
        text: "new answer".into(),
        base_char_offset: 100,
        total_chars: 110,
        reasoning_ranges: vec![],
        presentation: Default::default(),
        ack: None,
    });
    assert_eq!(ui.view().nodes()[0].reasoning_ranges, vec![(0, 5)]);
    assert_eq!(ui.view().anchor.char_offset, 2);
    let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(!screen_text(&terminal).contains("推論秘密文"));
}

#[test]
fn reasoning_metadata_capacity_counts_towards_nonvisible_body_cache() {
    let mut config = Config::default();
    config.cache.body_bytes = 512;
    let mut ui = UiState::new(&config, cwd());
    ui.select_thread("old".into());
    let generation = ui.view().generation;
    let old = ui.active.clone();
    let mut ranges = Vec::with_capacity(4096);
    ranges.push((0, 1));
    ui.apply(UiUpdate::History {
        thread_id: "old".into(),
        generation,
        nodes: vec![UiNode {
            presentation: Default::default(),
            node_id: "n".into(),
            role: "assistant".into(),
            text: "x".into(),
            base_char_offset: 0,
            total_chars: 1,
            reasoning_ranges: ranges,
        }],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 0,
    });
    assert!(!ui.view().nodes().is_empty());
    ui.new_chat();
    ui.apply(UiUpdate::Status {
        thread_id: None,
        text: "cache tick".into(),
    });
    assert_eq!(ui.views[&old].nodes().len(), 1);
    assert!(ui.views[&old].nodes()[0].text.is_empty());
    assert!(ui.views[&old].nodes()[0].reasoning_ranges.is_empty());
}

#[test]
fn tail_follow_uses_rendered_word_wrapping_for_long_context_messages() {
    let mut ui = state();
    ui.select_thread("t".into());
    let generation = ui.view().generation;
    let mut nodes: Vec<UiNode> = (0..5)
        .map(|i| {
            let text = format!(
                "context event {i}: {}",
                "{\"state\":\"word wrapping with 日本語 and a long context value\"} ".repeat(8)
            );
            UiNode {
                presentation: Default::default(),
                node_id: format!("event-{i}"),
                role: "developer".into(),
                total_chars: text.chars().count(),
                text,
                base_char_offset: 0,
                reasoning_ranges: vec![],
            }
        })
        .collect();
    nodes.push(UiNode {
        presentation: Default::default(),
        node_id: "final".into(),
        role: "assistant".into(),
        text: "NEWEST_ANSWER_VISIBLE".into(),
        base_char_offset: 0,
        total_chars: 21,
        reasoning_ranges: vec![],
    });
    ui.apply(UiUpdate::History {
        thread_id: "t".into(),
        generation,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 0,
    });
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(screen_text(&terminal).contains("NEWEST_ANSWER_VISIBLE"));
}

#[test]
fn accepted_snapshot_releases_body_and_restores_from_correlated_immutable_source() {
    let mut ui = state();
    let original = "原文😀\n".repeat(1000);
    ui.event(Event::Paste(original.clone()));
    let actions = enter(&mut ui);
    let (draft_id, revision) = sent(&actions);
    ui.apply(UiUpdate::Receipt {
        draft_id: draft_id.clone(),
        revision,
        thread_id: Some("thread".into()),
        status: ReceiptStatus::Accepted,
        message: None,
    });
    ui.apply(UiUpdate::ReceiptSource {
        draft_id: draft_id.clone(),
        revision,
        node_id: "immutable-message".into(),
    });
    let pending = &ui.view().pending[&revision];
    assert!(!pending.snapshot.folds.is_empty());
    let original_folds = pending.snapshot.folds.clone();
    assert_eq!(pending.snapshot.text.capacity(), 0);
    assert!(pending.snapshot.attachments.is_empty());
    assert_eq!(pending.source_node_id.as_deref(), Some("immutable-message"));
    ui.apply(UiUpdate::Receipt {
        draft_id: draft_id.clone(),
        revision,
        thread_id: None,
        status: ReceiptStatus::Cancelled,
        message: None,
    });
    text(&mut ui, &format!("/restore {revision}"));
    let actions = enter(&mut ui);
    let expected_revision = match command_action(&actions[0]) {
        UiAction::RestoreSnapshot {
            node_id,
            expected_revision,
            ..
        } => {
            assert_eq!(node_id, "immutable-message");
            *expected_revision
        }
        other => panic!("{other:?}"),
    };
    ui.apply(UiUpdate::RestoredSnapshot {
        draft_id: draft_id.clone(),
        revision,
        expected_revision,
        text: original.clone(),
        attachments: vec![],
    });
    assert_eq!(ui.view().draft.text, original);
    assert_eq!(ui.view().draft.folds.len(), original_folds.len());
    assert_eq!(ui.view().draft.folds[0].start, original_folds[0].start);
    assert_eq!(ui.view().draft.folds[0].end, original_folds[0].end);
    assert_eq!(ui.view().draft.cursor, original.len());
    text(&mut ui, "new typing");
    let edited = ui.view().draft.text.clone();
    ui.apply(UiUpdate::RestoredSnapshot {
        draft_id,
        revision,
        expected_revision,
        text: "stale".into(),
        attachments: vec![],
    });
    assert_eq!(ui.view().draft.text, edited);
}

#[test]
fn evicted_history_metadata_recovers_previous_and_next_ordinals() {
    let mut config = Config::default();
    // A large replay budget cannot retain UI metadata past body_bytes.
    config.cache.body_bytes = 1;
    let mut ui = UiState::new(&config, cwd());
    ui.select_thread("thread".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::History {
        thread_id: "thread".into(),
        generation,
        nodes: (0..10)
            .map(|i| UiNode {
                presentation: Default::default(),
                node_id: format!("node-{i}"),
                role: "assistant".into(),
                text: "日本語".into(),
                base_char_offset: 0,
                total_chars: 3,
                reasoning_ranges: vec![],
            })
            .collect(),
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 7,
        total_nodes: 10,
    });
    assert_eq!(ui.view().nodes().len(), 3);
    assert_eq!(ui.view().first_ordinal, 7);
    ui.view_mut().follow = false;
    ui.view_mut().anchor = ViewAnchor {
        node_id: Some("node-7".into()),
        char_offset: 0,
    };
    let actions = ui.event(key(KeyCode::PageUp));
    assert!(matches!(
        &actions[0],
        UiAction::LoadHistoryWindow {
            ordinal: 6,
            from_end: true,
            selection_revision: 7,
            ..
        }
    ));
    let view_revision = ui.view().view_revision;
    // Opposite input while the boundary read is pending cancels its residual
    // page displacement without invalidating the accepted source-window reply.
    assert!(ui.event(key(KeyCode::PageDown)).is_empty());
    ui.apply(UiUpdate::HistoryWindow {
        thread_id: "thread".into(),
        generation,
        view_revision,
        nodes: vec![UiNode {
            presentation: Default::default(),
            node_id: "node-6".into(),
            role: "assistant".into(),
            text: "日本語".into(),
            base_char_offset: 0,
            total_chars: 3,
            reasoning_ranges: vec![],
        }],
        first_ordinal: 6,
        selection_revision: 7,
        total_nodes: 10,
        anchor: ViewAnchor {
            node_id: Some("node-6".into()),
            char_offset: 0,
        },
    });
    let actions = ui.event(key(KeyCode::PageDown));
    assert!(matches!(
        &actions[0],
        UiAction::LoadHistoryWindow {
            ordinal: 7,
            from_end: false,
            ..
        }
    ));
}

#[test]
fn evicted_body_keeps_node_identity_and_reloads_at_original_character_offset() {
    let mut config = Config::default();
    // Retain the small navigation metadata after evicting the large bodies.
    config.cache.body_bytes = 2048;
    let mut ui = UiState::new(&config, cwd());
    ui.select_thread("thread".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::History {
        thread_id: "thread".into(),
        generation,
        nodes: (0..5)
            .map(|i| UiNode {
                presentation: Default::default(),
                node_id: format!("node-{i}"),
                role: "assistant".into(),
                text: "界".repeat(10000),
                base_char_offset: 0,
                total_chars: 10000,
                reasoning_ranges: vec![],
            })
            .collect(),
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 3,
        total_nodes: 5,
    });
    assert_eq!(ui.view().nodes().len(), 5);
    assert!(ui.view().nodes()[0].text.is_empty());
    assert_eq!(ui.view().nodes()[0].total_chars, 10000);
    ui.view_mut().follow = false;
    ui.view_mut().anchor = ViewAnchor {
        node_id: Some("node-0".into()),
        char_offset: 10,
    };
    let actions = ui.event(key(KeyCode::PageDown));
    let offset = match &actions[0] {
        UiAction::LoadNodeWindow {
            node_id,
            char_offset,
            ..
        } => {
            assert_eq!(node_id, "node-0");
            *char_offset
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(
        offset, 10,
        "load the durable source anchor before applying row movement"
    );
    let view_revision = ui.view().view_revision;
    ui.apply(UiUpdate::NodeWindow {
        thread_id: "thread".into(),
        generation,
        view_revision,
        node: UiNode {
            presentation: Default::default(),
            node_id: "node-0".into(),
            role: "assistant".into(),
            text: "界".repeat(10000),
            base_char_offset: 0,
            total_chars: 10000,
            reasoning_ranges: vec![],
        },
    });
    assert!(
        ui.view().anchor.char_offset > offset,
        "apply the queued page as wrapped rows after its body arrives"
    );
    assert_eq!(ui.view().nodes()[0].text, "界".repeat(10000));
}

#[test]
fn zero_body_budget_releases_page_allocations_and_preserves_three_working_nodes() {
    let mut config = Config::default();
    config.cache.body_bytes = 0;
    let mut ui = UiState::new(&config, cwd());
    ui.select_thread("zero".into());
    let key = ui.active.clone();
    let generation = ui.view().generation;
    let mut nodes = Vec::with_capacity(4096);
    for i in 0..10 {
        nodes.push(UiNode {
            presentation: Default::default(),
            node_id: format!("zero-{i}"),
            role: "assistant".into(),
            text: "retained working text".into(),
            base_char_offset: 0,
            total_chars: 21,
            reasoning_ranges: vec![],
        });
    }
    ui.apply(UiUpdate::History {
        thread_id: "zero".into(),
        generation,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 5,
        total_nodes: 10,
    });
    assert_eq!(ui.view().nodes().len(), 3);
    assert_eq!(ui.view().nodes().capacity(), 3);
    assert_eq!(ui.view().first_ordinal, 7);
    assert!(ui.view().nodes().iter().all(|n| !n.text.is_empty()));
    ui.new_chat();
    ui.apply(UiUpdate::Status {
        thread_id: None,
        text: "trim inactive".into(),
    });
    assert_eq!(ui.views[&key].nodes().capacity(), 0);
    assert_eq!(ui.views[&key].first_ordinal, 7);
    assert_eq!(ui.views[&key].selection_revision, 5);
}

#[test]
fn cached_history_render_replaces_same_length_source_and_recomputes_resize() {
    let mut ui = state();
    ui.select_thread("render".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::History {
        thread_id: "render".into(),
        generation,
        nodes: vec![UiNode {
            presentation: Default::default(),
            node_id: "n".into(),
            role: "assistant".into(),
            text: "AAAA 日本語".into(),
            base_char_offset: 0,
            total_chars: 8,
            reasoning_ranges: vec![],
        }],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 1,
    });
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(screen_text(&terminal).contains("AAAA 日本語"));
    let view_revision = ui.view().view_revision;
    ui.apply(UiUpdate::NodeWindow {
        thread_id: "render".into(),
        generation,
        view_revision,
        node: UiNode {
            presentation: Default::default(),
            node_id: "n".into(),
            role: "assistant".into(),
            text: "BBBB 日本語".into(),
            base_char_offset: 0,
            total_chars: 8,
            reasoning_ranges: vec![],
        },
    });
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(screen_text(&terminal).contains("BBBB 日本語"));
    assert!(!screen_text(&terminal).contains("AAAA"));
    let mut narrow = Terminal::new(TestBackend::new(24, 16)).unwrap();
    narrow.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(screen_text(&narrow).contains("BBBB 日本語"));
}

#[test]
fn typed_conversation_has_inline_markers_and_details_copy_full_urls_without_changing_draft_anchor()
{
    use shellboy::presentation::project;
    let mut ui = state();
    ui.select_thread("thread".into());
    let value = "# 日本語の調査\n\n文章と[参照](https://example.test/complete/path?key=value)。";
    let node_id = "00000000-0000-4000-8000-000000000001";
    let mut presentation = project(
        "assistant_response",
        &serde_json::json!({"role":"assistant","content":[{"type":"output_text","text":value}]}),
        value,
        0,
    );
    shellboy::presentation::render_markdown(&mut presentation, 60);
    ui.apply(UiUpdate::History {
        thread_id: "thread".into(),
        generation: ui.view().generation,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 1,
        nodes: vec![UiNode {
            node_id: node_id.into(),
            role: "assistant".into(),
            text: value.into(),
            base_char_offset: 0,
            total_chars: value.chars().count(),
            reasoning_ranges: vec![],
            presentation,
        }],
    });
    text(&mut ui, "保存する下書き");
    let anchor = ui.view().anchor.clone();
    let draft = ui.view().draft.clone();
    let mut terminal = Terminal::new(TestBackend::new(64, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let normal = screen_text(&terminal);
    assert!(normal.contains("● 日本語の調査"));
    assert!(!normal.contains(node_id));
    assert!(!normal.contains("あなた\n"));
    ui.event(key(KeyCode::F(7)));
    let copied = ui.event(key(KeyCode::Char('c')));
    assert!(
        matches!(&copied[..], [UiAction::CopyText{text,..}] if text == "https://example.test/complete/path?key=value"),
        "{copied:?}"
    );
    ui.event(key(KeyCode::Esc));
    ui.event(key(KeyCode::F(6)));
    ui.event(key(KeyCode::Enter));
    let original = ui.event(key(KeyCode::Char('o')));
    let request_id = match &original[..] {
        [
            UiAction::LoadOriginal {
                request_id,
                node_id: id,
                cursor: None,
            },
        ] if id == node_id => *request_id,
        _ => panic!("original action"),
    };
    let body = shellboy::protocol::HistoryBody {
        context: serde_json::from_value(serde_json::json!({"state":"present","node_id":node_id,"created_at":"2026-10-25T00:00:00Z"})).unwrap(),
        provenance: shellboy_provenance_fixture(),
        node_id: node_id.into(),
        kind: "assistant_response".into(),
        text: "current-page".into(),
        source_node_ids: vec![],
        previous_node_ids: vec![],
        next_node_ids: vec![],
        next_cursor: Some("next".into()),
        tombstone_reason: None,
    };
    ui.apply(UiUpdate::original_page(request_id + 1, body.clone()));
    assert!(ui.event(key(KeyCode::Char('c'))).is_empty());
    ui.apply(UiUpdate::original_page(request_id, body.clone()));

    assert!(
        matches!(&ui.event(key(KeyCode::Char('c')))[..], [UiAction::CopyText{text,scope:shellboy::ui::CopyScope::OriginalPage{has_more:true}}] if text == "current-page")
    );
    let next = ui.event(key(KeyCode::Char('n')));
    let actions = command_actions(&next);
    let next_id = match &actions[..] {
        [
            UiAction::LoadOriginal {
                cursor: Some(cursor),
                request_id,
                ..
            },
        ] if cursor == "next" => *request_id,
        _ => panic!("next original page"),
    };
    let mut last = body;
    last.text = "last-page".into();
    last.next_cursor = None;
    ui.apply(UiUpdate::original_page(next_id, last));
    assert!(
        matches!(&ui.event(key(KeyCode::Char('c')))[..], [UiAction::CopyText { text, scope: shellboy::ui::CopyScope::OriginalPage { has_more: false } }] if text == "last-page")
    );
    ui.event(key(KeyCode::Esc));
    assert_eq!(ui.view().draft, draft);
    assert_eq!(ui.view().anchor, anchor);
}

#[test]
fn structured_management_focus_pages_and_wait_cancel_use_typed_worker_actions() {
    let mut ui = state();
    ui.apply(UiUpdate::StructuredManagement {
        work_status: false,
        request_id: 7,
        title: "待機結果".into(),
        fields: vec![shellboy::presentation::PresentationField {
            value_omitted: false,
            display: None,
            key: "".into(),
            label: "全結果の詳細".into(),
            value: "原文取得".into(),
        }],
        omitted: true,
    });
    assert!(
        matches!(&command_actions(&ui.event(key(KeyCode::Enter)))[..], [UiAction::LoadManagementDetail{request_id:7,pointer,cursor:None}] if pointer.is_empty())
    );
    ui.apply(UiUpdate::ManagementDetail {
        request_id: 6,
        pointer: "".into(),
        text: "stale".into(),
        next_cursor: None,
    });
    ui.apply(UiUpdate::ManagementDetail {
        request_id: 7,
        pointer: "".into(),
        text: "complete value".into(),
        next_cursor: Some("next".into()),
    });
    assert!(
        matches!(&ui.event(key(KeyCode::Char('c')))[..], [UiAction::CopyText{text,scope:shellboy::ui::CopyScope::ManagementPage{has_more:true}}] if text == "complete value")
    );
    let next = ui.event(key(KeyCode::Char('n')));
    assert!(
        matches!(&command_actions(&next)[..], [UiAction::LoadManagementDetail { cursor: Some(cursor), .. }] if cursor == "next")
    );
    ui.apply(UiUpdate::ManagementDetail {
        request_id: 7,
        pointer: "".into(),
        text: "last detail page".into(),
        next_cursor: None,
    });
    assert!(
        matches!(&ui.event(key(KeyCode::Char('c')))[..], [UiAction::CopyText { text, scope: shellboy::ui::CopyScope::ManagementPage { has_more: false } }] if text == "last detail page")
    );
    assert!(matches!(
        &ui.event(key(KeyCode::Char('w')))[..],
        [UiAction::CancelWait]
    ));
}

#[test]
fn work_status_uses_read_actions_and_preserves_the_conversation_draft() {
    use shellboy::{presentation::PresentationField, protocol::*};
    let mut ui = state();
    text(&mut ui, "/status");
    assert!(
        matches!(command_action(&enter(&mut ui)[0]),UiAction::Manage(Command::Status(a)) if !a.all && a.thread_id.is_none())
    );
    ui.apply(UiUpdate::StructuredManagement {
        work_status: true,
        request_id: 88,
        title: "作業状態".into(),
        fields: vec![
            PresentationField {
                value_omitted: false,
                display: None,
                key: "/items/0".into(),
                label: "shell running".into(),
                value: "調査 · 01234567".into(),
            },
            PresentationField {
                value_omitted: false,
                display: None,
                key: "".into(),
                label: "全詳細".into(),
                value: "観測範囲".into(),
            },
        ],
        omitted: false,
    });
    let draft = ui.view().draft.clone();
    let anchor = ui.view().anchor.clone();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let rendered = screen_text(&terminal);
    assert!(rendered.contains("shell running") && rendered.contains("調査"));
    assert!(!rendered.contains("protocol_version"));
    assert!(
        matches!(&command_actions(&ui.event(key(KeyCode::Char('o'))))[..],[UiAction::FollowManagement{request_id:88,pointer,mode:ManagementFollow::Inspect}] if pointer=="/items/0")
    );
    assert!(matches!(
        &command_actions(&ui.event(key(KeyCode::Char('s'))))[..],
        [UiAction::FollowManagement {
            mode: ManagementFollow::Read,
            ..
        }]
    ));
    assert!(matches!(
        &command_actions(&ui.event(key(KeyCode::Char('n'))))[..],
        [UiAction::FollowManagement {
            mode: ManagementFollow::Next,
            ..
        }]
    ));
    assert!(matches!(
        &command_actions(&ui.event(key(KeyCode::Char('r'))))[..],
        [UiAction::FollowManagement {
            mode: ManagementFollow::Refresh,
            ..
        }]
    ));
    ui.event(key(KeyCode::Down));
    assert!(
        matches!(&command_actions(&ui.event(key(KeyCode::Enter)))[..],[UiAction::LoadManagementDetail{pointer,..}] if pointer.is_empty())
    );
    ui.event(key(KeyCode::Esc));
    assert_eq!(ui.view().draft.text, draft.text);
    assert_eq!(ui.view().anchor, anchor);
}

#[test]
fn work_status_current_scope_requires_a_saved_thread() {
    let mut ui = state();
    text(&mut ui, "/status thread");
    assert!(enter(&mut ui).is_empty());
    assert!(ui.view().status.contains("未保存"));
}

#[test]
fn markdown_resize_and_internal_source_anchor_request_bounded_worker_reprojection() {
    let mut ui = state();
    ui.select_thread("thread".into());
    let original = "前の段落。\n\n# 中央の見出し\n\n後の本文。";
    let offset = "前の段落。\n\n".chars().count();
    let payload =
        serde_json::json!({"role":"assistant","content":[{"type":"output_text","text":original}]});
    let mut projection =
        shellboy::presentation::project("assistant_response", &payload, original, 0);
    shellboy::presentation::render_markdown(&mut projection, 60);
    ui.apply(UiUpdate::History {
        thread_id: "thread".into(),
        generation: ui.view().generation,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 1,
        nodes: vec![UiNode {
            node_id: "node".into(),
            role: "assistant".into(),
            text: original.into(),
            base_char_offset: 0,
            total_chars: original.chars().count(),
            reasoning_ranges: vec![],
            presentation: projection,
        }],
    });
    ui.view_mut().follow = false;
    ui.view_mut().anchor = ViewAnchor {
        node_id: Some("node".into()),
        char_offset: offset,
    };
    let mut terminal = Terminal::new(TestBackend::new(64, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let actions = ui.take_deferred_actions();
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, UiAction::RenderWidth { width: 60 }))
    );
    assert_eq!(actions.iter().filter(|action| matches!(action,UiAction::LoadNodeWindow{node_id,char_offset,..} if node_id == "node" && *char_offset == offset)).count(),1);
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(ui.take_deferred_actions().is_empty());
    let slice = original.chars().skip(offset).collect::<String>();
    let mut projected =
        shellboy::presentation::project("assistant_response", &payload, &slice, offset);
    shellboy::presentation::render_markdown(&mut projected, 60);
    ui.apply(UiUpdate::NodeWindow {
        thread_id: "thread".into(),
        generation: ui.view().generation,
        view_revision: ui.view().view_revision,
        node: UiNode {
            node_id: "node".into(),
            role: "assistant".into(),
            text: slice.clone(),
            base_char_offset: offset,
            total_chars: original.chars().count(),
            reasoning_ranges: vec![],
            presentation: projected,
        },
    });
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let shown = screen_text(&terminal);
    assert!(shown.contains("中央の見出し"));
    assert!(!shown.contains("# 中央"));
    assert!(ui.take_deferred_actions().is_empty());
    let mut narrower = Terminal::new(TestBackend::new(48, 24)).unwrap();
    narrower.draw(|frame| draw(frame, &mut ui)).unwrap();
    let actions = ui.take_deferred_actions();
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, UiAction::RenderWidth { width: 44 }))
    );
    assert!(actions.iter().any(
        |action| matches!(action,UiAction::LoadNodeWindow{char_offset,..} if *char_offset == offset)
    ));
    assert_eq!(ui.view().anchor.char_offset, offset);
}

fn scroll_fixture_node(index: usize, body: String) -> UiNode {
    UiNode {
        node_id: format!("scroll-{index}"),
        role: "assistant".into(),
        total_chars: body.chars().count(),
        text: body,
        base_char_offset: 0,
        reasoning_ranges: vec![],
        presentation: Default::default(),
    }
}
fn scroll_fixture(nodes: Vec<UiNode>, first: u64, total: u64) -> UiState {
    let mut ui = state();
    ui.select_thread("scroll-fixture".into());
    ui.apply(UiUpdate::History {
        thread_id: "scroll-fixture".into(),
        generation: ui.view().generation,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal: first,
        selection_revision: 7,
        total_nodes: total,
    });
    ui
}
fn scroll_wheel(ui: &mut UiState, up: bool) -> Vec<UiAction> {
    use crossterm::event::{MouseEvent, MouseEventKind};
    ui.event(Event::Mouse(MouseEvent {
        kind: if up {
            MouseEventKind::ScrollUp
        } else {
            MouseEventKind::ScrollDown
        },
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }))
}
fn scroll_cells(ui: &mut UiState, terminal: &mut Terminal<TestBackend>) -> Vec<String> {
    terminal.draw(|frame| draw(frame, ui)).unwrap();
    let buffer = terminal.backend().buffer();
    (2..buffer.area.height - 6)
        .map(|y| {
            let mut out = String::new();
            let mut x = 1;
            while x < buffer.area.width - 1 {
                let symbol = buffer[(x, y)].symbol();
                out.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            out
        })
        .collect()
}
#[test]
fn wheel_burst_preserves_rows_across_overlapping_async_history_windows_and_prefetch() {
    let all = (0..120)
        .map(|i| scroll_fixture_node(i, format!("M{i:03}")))
        .collect::<Vec<_>>();
    let mut expected = scroll_fixture(all.clone(), 0, 120);
    let mut actual = scroll_fixture(all[88..].to_vec(), 88, 120);
    let mut a = Terminal::new(TestBackend::new(60, 28)).unwrap();
    let mut b = Terminal::new(TestBackend::new(60, 28)).unwrap();
    assert_eq!(
        scroll_cells(&mut actual, &mut a),
        scroll_cells(&mut expected, &mut b)
    );
    let mut requests = std::collections::VecDeque::new();
    for _ in 0..40 {
        requests.extend(scroll_wheel(&mut actual, true));
        assert!(scroll_wheel(&mut expected, true).is_empty());
    }
    let mut reads = 0;
    while let Some(action) = requests.pop_front() {
        if let UiAction::LoadHistoryWindow {
            thread_id,
            generation,
            view_revision,
            ordinal,
            char_offset,
            from_end,
            selection_revision,
        } = action
        {
            reads += 1;
            assert!(reads < 12, "each short read must make finite progress");
            // Worker-produced Markdown prefetch uses the same public reply type,
            // but is not the navigation read and must not consume its delta.
            actual.apply(UiUpdate::NodeWindow {
                thread_id: thread_id.clone(),
                generation,
                view_revision,
                node: actual.view().nodes().last().unwrap().clone(),
            });
            assert!(actual.view().loading);
            let first = ordinal.saturating_sub(10) as usize;
            let nodes = all[first..(first + 50).min(all.len())].to_vec();
            let count = all[ordinal as usize].total_chars;
            let offset = if from_end {
                count.saturating_sub(char_offset)
            } else {
                char_offset.min(count)
            };
            actual.apply(UiUpdate::HistoryWindow {
                thread_id,
                generation,
                view_revision,
                nodes,
                first_ordinal: first as u64,
                selection_revision,
                total_nodes: 120,
                anchor: ViewAnchor {
                    node_id: Some(all[ordinal as usize].node_id.clone()),
                    char_offset: offset,
                },
            });
            requests.extend(actual.take_deferred_actions());
        }
    }
    assert!(reads > 1);
    assert!(!actual.view().loading);
    assert_eq!(
        scroll_cells(&mut actual, &mut a),
        scroll_cells(&mut expected, &mut b)
    );
    assert_eq!(actual.view().anchor, expected.view().anchor);
}
#[test]
fn wheel_during_initial_loading_cannot_replay_on_unrelated_node_prefetch() {
    let mut ui = state();
    ui.select_thread("scroll-fixture".into());
    assert!(scroll_wheel(&mut ui, true).is_empty());
    ui.apply(UiUpdate::History {
        thread_id: "scroll-fixture".into(),
        generation: ui.view().generation,
        nodes: vec![scroll_fixture_node(
            0,
            (0..100).map(|i| format!("L{i:03}\n")).collect(),
        )],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 1,
    });
    let mut terminal = Terminal::new(TestBackend::new(60, 28)).unwrap();
    let before = scroll_cells(&mut ui, &mut terminal);
    ui.apply(UiUpdate::NodeWindow {
        thread_id: "scroll-fixture".into(),
        generation: ui.view().generation,
        view_revision: ui.view().view_revision,
        node: ui.view().nodes()[0].clone(),
    });
    assert_eq!(scroll_cells(&mut ui, &mut terminal), before);
    assert!(ui.view().follow);
}
#[test]
fn more_than_u16_wrapped_rows_draw_the_actual_tail_without_scroll_clamping() {
    let nodes = (0..3)
        .map(|i| scroll_fixture_node(i, format!("{}TAIL{i}", "z\n".repeat(30_000))))
        .collect();
    let mut ui = scroll_fixture(nodes, 0, 3);
    let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(screen_text(&terminal).contains("TAIL2"));
    assert!(scroll_wheel(&mut ui, true).is_empty());
    assert!(!ui.view().follow);
    assert!(scroll_wheel(&mut ui, false).is_empty());
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(screen_text(&terminal).contains("TAIL2"));
    assert!(ui.view().follow);
}

#[test]
fn long_folded_reasoning_and_inline_markdown_keep_original_body_position_on_resize() {
    use serde_json::json;
    use shellboy::{
        history::text_of_payload,
        presentation::{project, render_markdown},
    };
    let reasoning = "非表示の推論。".repeat(1200);
    let body = (0..28)
        .map(|i| {
            format!(
                "P{i:03} **太字** と *強調* と [参照](https://example.test/{i}) を含む段落です。 "
            )
        })
        .collect::<String>();
    let payload = json!({"output":[
        {"type":"reasoning","summary":[{"type":"summary_text","text":reasoning}]},
        {"type":"message","content":[{"type":"output_text","text":body}]}]});
    let original = text_of_payload(&payload);
    let mut node = scroll_fixture_node(0, original.clone());
    node.reasoning_ranges = vec![(0, reasoning.chars().count())];
    node.presentation = project("assistant_response", &payload, &original, 0);
    render_markdown(&mut node.presentation, 56);
    let mut ui = scroll_fixture(vec![node.clone()], 0, 1);
    let mut terminal = Terminal::new(TestBackend::new(60, 28)).unwrap();
    scroll_cells(&mut ui, &mut terminal);
    assert!(scroll_wheel(&mut ui, true).is_empty());
    scroll_cells(&mut ui, &mut terminal);
    let anchor = ui.view().anchor.clone();
    assert!(
        anchor.char_offset > reasoning.chars().count(),
        "body rows must skip the hidden source gap"
    );
    assert!(
        anchor.char_offset > reasoning.chars().count() + body.chars().count() / 3,
        "inline spans must advance past preceding rows"
    );
    render_markdown(&mut node.presentation, 44);
    ui.apply(UiUpdate::NodeWindow {
        thread_id: "scroll-fixture".into(),
        generation: ui.view().generation,
        view_revision: ui.view().view_revision,
        node,
    });
    terminal.backend_mut().resize(48, 28);
    scroll_cells(&mut ui, &mut terminal);
    assert_eq!(ui.view().anchor, anchor);
    assert!(!screen_text(&terminal).contains("非表示の推論。"));
    assert_eq!(ui.view().nodes()[0].text, original);
    assert!(!ui.view().follow);
}
#[test]
fn derived_tool_excerpt_moves_by_visible_rows() {
    use serde_json::json;
    use shellboy::{history::text_of_payload, presentation::project};
    let output = (0..60)
        .map(|i| format!("O{i:03} {}\n", "long output ".repeat(30)))
        .collect::<String>();
    let payload = json!({"item":{"type":"function_call_output","call_id":"call",
        "output":json!({"status":"completed","exit_code":0,"output_complete":true,"output":output}).to_string()}});
    let original = text_of_payload(&payload);
    let mut node = scroll_fixture_node(0, original.clone());
    node.role = "tool".into();
    node.presentation = project("tool_result", &payload, &original, 0);
    let mut ui = scroll_fixture(vec![node], 0, 1);
    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let before = scroll_cells(&mut ui, &mut terminal);
    scroll_wheel(&mut ui, true);
    let up = scroll_cells(&mut ui, &mut terminal);
    assert_eq!(&up[3..], &before[..before.len() - 3]);
    scroll_wheel(&mut ui, false);
    assert_eq!(scroll_cells(&mut ui, &mut terminal), before);
    assert_eq!(ui.view().nodes()[0].text, original);
    assert!(ui.view().follow);
}

#[test]
fn body_window_boundaries_replay_visible_rows_with_real_frontend_overlap() {
    let source = (0..20_000)
        .map(|i| format!("N{i:05}\n"))
        .collect::<String>();
    fn window(source: &str, offset: usize) -> UiNode {
        let start = source
            .char_indices()
            .nth(offset.min(source.chars().count()).saturating_sub(512))
            .map_or(source.len(), |(byte, _)| byte);
        let mut end = (start + 65_536).min(source.len());
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        let mut node = scroll_fixture_node(0, source[start..end].to_owned());
        node.base_char_offset = source[..start].chars().count();
        node.total_chars = source.chars().count();
        node
    }
    for up in [true, false] {
        let initial = window(&source, 40_000);
        let first = initial.base_char_offset;
        let end = first + initial.text.chars().count();
        let offset = if up { first + 20 } else { end - 220 };
        let offset = offset / 7 * 7;
        let mut actual = scroll_fixture(vec![initial], 0, 1);
        let mut expected = scroll_fixture(vec![scroll_fixture_node(0, source.clone())], 0, 1);
        for ui in [&mut actual, &mut expected] {
            ui.view_mut().anchor = ViewAnchor {
                node_id: Some("scroll-0".into()),
                char_offset: offset,
            };
            ui.view_mut().follow = false;
        }
        let mut a = Terminal::new(TestBackend::new(60, 28)).unwrap();
        let mut b = Terminal::new(TestBackend::new(60, 28)).unwrap();
        assert_eq!(
            scroll_cells(&mut actual, &mut a),
            scroll_cells(&mut expected, &mut b)
        );
        let mut requests = std::collections::VecDeque::new();
        let page = if up {
            KeyCode::PageUp
        } else {
            KeyCode::PageDown
        };
        requests.extend(actual.event(key(page)));
        expected.event(key(page));
        for _ in 0..2 {
            requests.extend(scroll_wheel(&mut actual, up));
            expected.event(crossterm::event::Event::Mouse(
                crossterm::event::MouseEvent {
                    kind: if up {
                        crossterm::event::MouseEventKind::ScrollUp
                    } else {
                        crossterm::event::MouseEventKind::ScrollDown
                    },
                    column: 10,
                    row: 5,
                    modifiers: KeyModifiers::NONE,
                },
            ));
        }
        let mut reads = 0;
        while let Some(action) = requests.pop_front() {
            if let UiAction::LoadNodeWindow {
                thread_id,
                generation,
                view_revision,
                node_id: _,
                char_offset,
            } = action
            {
                reads += 1;
                assert!(reads < 12);
                actual.apply(UiUpdate::NodeWindow {
                    thread_id,
                    generation,
                    view_revision,
                    node: window(&source, char_offset),
                });
                requests.extend(actual.take_deferred_actions());
            }
        }
        assert!(reads > 0, "fixture must cross a body window: up={up}");
        assert_eq!(
            scroll_cells(&mut actual, &mut a),
            scroll_cells(&mut expected, &mut b),
            "up={up}"
        );
        assert_eq!(actual.view().anchor, expected.view().anchor, "up={up}");
        assert!(!actual.view().loading);
    }
}

#[test]
fn late_body_reply_cannot_append_an_older_node_after_the_current_path_window() {
    let nodes = (50..55)
        .map(|i| scroll_fixture_node(i, format!("CURRENT {i}\n")))
        .collect();
    let mut ui = scroll_fixture(nodes, 50, 55);
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    let before = scroll_cells(&mut ui, &mut terminal);
    scroll_wheel(&mut ui, false);
    ui.apply(UiUpdate::NodeWindow {
        thread_id: "scroll-fixture".into(),
        generation: ui.view().generation,
        view_revision: ui.view().view_revision,
        node: scroll_fixture_node(5, "OLD HISTORY MUST NOT BECOME THE TAIL\n".into()),
    });
    assert_eq!(ui.view().nodes().last().unwrap().node_id, "scroll-54");
    assert_eq!(scroll_cells(&mut ui, &mut terminal), before);
    assert_eq!(ui.view().first_ordinal, 50);
    assert_eq!(ui.view().nodes().len(), 5);
}

#[test]
fn slow_downward_wheels_remain_monotonic_across_settled_body_windows() {
    let source = (0..20_000)
        .map(|i| format!("N{i:05}\n"))
        .collect::<String>();
    let window = |offset: usize| {
        let start = offset.min(source.len()).saturating_sub(512);
        let end = (start + 65_536).min(source.len());
        let mut node = scroll_fixture_node(0, source[start..end].to_owned());
        node.base_char_offset = start;
        node.total_chars = source.len();
        node
    };
    let mut actual = scroll_fixture(vec![window(40_000)], 0, 1);
    let mut expected = scroll_fixture(vec![scroll_fixture_node(0, source.clone())], 0, 1);
    for ui in [&mut actual, &mut expected] {
        ui.view_mut().follow = false;
        ui.view_mut().anchor = ViewAnchor {
            node_id: Some("scroll-0".into()),
            char_offset: 104_650,
        };
    }
    let mut a = Terminal::new(TestBackend::new(60, 28)).unwrap();
    let mut b = Terminal::new(TestBackend::new(60, 28)).unwrap();
    assert_eq!(
        scroll_cells(&mut actual, &mut a),
        scroll_cells(&mut expected, &mut b)
    );
    for step in 0..80 {
        let mut reads = std::collections::VecDeque::from(scroll_wheel(&mut actual, false));
        assert!(scroll_wheel(&mut expected, false).is_empty());
        while let Some(action) = reads.pop_front() {
            if let UiAction::LoadNodeWindow {
                thread_id,
                generation,
                view_revision,
                char_offset,
                ..
            } = action
            {
                actual.apply(UiUpdate::NodeWindow {
                    thread_id,
                    generation,
                    view_revision,
                    node: window(char_offset),
                });
                reads.extend(actual.take_deferred_actions());
            }
        }
        assert_eq!(
            scroll_cells(&mut actual, &mut a),
            scroll_cells(&mut expected, &mut b),
            "settled wheel {step}"
        );
        assert_eq!(
            actual.view().anchor,
            expected.view().anchor,
            "settled wheel {step}"
        );
    }
}

#[test]
fn slow_forward_history_page_keeps_the_visible_rows_until_the_reply() {
    let all = (0..120)
        .map(|i| scroll_fixture_node(i, format!("N{i:03}\n")))
        .collect::<Vec<_>>();
    let mut actual = scroll_fixture(all[..50].to_vec(), 0, 120);
    let mut expected = scroll_fixture(all.clone(), 0, 120);
    for ui in [&mut actual, &mut expected] {
        ui.view_mut().follow = false;
        ui.view_mut().anchor = ViewAnchor {
            node_id: Some("scroll-30".into()),
            char_offset: 0,
        };
    }
    let mut a = Terminal::new(TestBackend::new(60, 48)).unwrap();
    let mut b = Terminal::new(TestBackend::new(60, 48)).unwrap();
    let before = scroll_cells(&mut actual, &mut a);
    scroll_cells(&mut expected, &mut b);
    let actions = scroll_wheel(&mut actual, false);
    scroll_wheel(&mut expected, false);
    assert!(actual.view().loading, "fixture must cross a metadata page");
    assert_eq!(
        scroll_cells(&mut actual, &mut a),
        before,
        "loading must not jump to the old page's final row"
    );
    let mut requests = std::collections::VecDeque::from(actions);
    let mut reads = 0;
    while let Some(action) = requests.pop_front() {
        if let UiAction::LoadHistoryWindow {
            thread_id,
            generation,
            view_revision,
            ordinal,
            selection_revision,
            ..
        } = action
        {
            reads += 1;
            assert!(reads < 4);
            let first = ordinal.saturating_sub(10) as usize;
            actual.apply(UiUpdate::HistoryWindow {
                thread_id,
                generation,
                view_revision,
                nodes: all[first..(first + 50).min(all.len())].to_vec(),
                first_ordinal: first as u64,
                selection_revision,
                total_nodes: 120,
                anchor: ViewAnchor {
                    node_id: Some(all[ordinal as usize].node_id.clone()),
                    char_offset: 0,
                },
            });
            assert_eq!(
                scroll_cells(&mut actual, &mut a),
                scroll_cells(&mut expected, &mut b),
                "a downward wheel must not expose a later checkpoint then jump backward"
            );
            requests.extend(actual.take_deferred_actions());
        }
    }
    assert_eq!(reads, 1);
    assert!(!actual.view().loading);
}

#[test]
fn restored_anchor_outside_the_cached_body_loads_before_any_scroll_input() {
    let mut node = scroll_fixture_node(0, "cached tail".into());
    node.base_char_offset = 90_000;
    node.total_chars = 90_011;
    let mut ui = scroll_fixture(vec![node], 0, 1);
    ui.apply(UiUpdate::ViewState {
        thread_id: "scroll-fixture".into(),
        generation: ui.view().generation,
        view_revision: ui.view().view_revision,
        anchor: ViewAnchor {
            node_id: Some("scroll-0".into()),
            char_offset: 65_000,
        },
        follow: false,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(
        ui.take_deferred_actions()
            .iter()
            .any(|action| matches!(action,
        UiAction::LoadNodeWindow {node_id,char_offset:65_000,..} if node_id=="scroll-0"))
    );
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(
        ui.take_deferred_actions().is_empty(),
        "one correlated request, not a redraw loop"
    );
}

#[test]
fn ninety_backward_rows_prefetch_one_window_and_show_loading_until_replay_finishes() {
    let source = (1..=2000)
        .map(|i| {
            format!(
                "B{i:05} bounded source window preserves the visible rows while loading more. \n"
            )
        })
        .collect::<String>();
    fn window(source: &str, offset: usize) -> UiNode {
        let start = source
            .char_indices()
            .nth(offset.min(source.chars().count()).saturating_sub(512))
            .map_or(source.len(), |(byte, _)| byte);
        let mut end = (start + 65_536).min(source.len());
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        let mut node = scroll_fixture_node(0, source[start..end].to_owned());
        node.base_char_offset = source[..start].chars().count();
        node.total_chars = source.chars().count();
        node
    }
    let mut actual = scroll_fixture(vec![window(&source, 65_000)], 0, 1);
    let mut expected = scroll_fixture(vec![scroll_fixture_node(0, source.clone())], 0, 1);
    for ui in [&mut actual, &mut expected] {
        ui.view_mut().anchor = ViewAnchor {
            node_id: Some("scroll-0".into()),
            char_offset: 65_000,
        };
        ui.view_mut().follow = false;
    }
    let mut a = Terminal::new(TestBackend::new(100, 36)).unwrap();
    let mut b = Terminal::new(TestBackend::new(100, 36)).unwrap();
    assert_eq!(
        scroll_cells(&mut actual, &mut a),
        scroll_cells(&mut expected, &mut b)
    );
    let mut requests = std::collections::VecDeque::new();
    for _ in 0..30 {
        requests.extend(scroll_wheel(&mut actual, true));
        assert!(scroll_wheel(&mut expected, true).is_empty());
    }
    assert!(actual.view().loading);
    a.draw(|frame| draw(frame, &mut actual)).unwrap();
    assert!(screen_text(&a).contains("読込み中"));
    let mut reads = 0;
    while let Some(action) = requests.pop_front() {
        if let UiAction::LoadNodeWindow {
            thread_id,
            generation,
            view_revision,
            char_offset,
            ..
        } = action
        {
            reads += 1;
            assert!(
                reads <= 2,
                "90 rows must not require fourteen serialized 513-character reads"
            );
            let loaded = window(&source, char_offset);
            assert!(loaded.base_char_offset < actual.view().nodes()[0].base_char_offset);
            actual.apply(UiUpdate::NodeWindow {
                thread_id,
                generation,
                view_revision,
                node: loaded,
            });
            requests.extend(actual.take_deferred_actions());
        }
    }
    assert_eq!(reads, 1);
    assert!(!actual.view().loading);
    assert_eq!(
        scroll_cells(&mut actual, &mut a),
        scroll_cells(&mut expected, &mut b)
    );
    assert_eq!(actual.view().anchor, expected.view().anchor);
    assert!(!screen_text(&a).contains("読込み中"));
}

#[test]
fn forward_page_loads_the_first_unknown_neighbor_before_using_later_rows() {
    let all = (0..10)
        .map(|i| {
            scroll_fixture_node(
                i,
                (0..100)
                    .map(|line| format!("N{i} row {line:03}\n"))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let mut nodes = all.clone();
    nodes[4].text.clear();
    let mut actual = scroll_fixture(nodes, 0, 10);
    let mut expected = scroll_fixture(all.clone(), 0, 10);
    for ui in [&mut actual, &mut expected] {
        ui.view_mut().follow = false;
        ui.view_mut().anchor = ViewAnchor {
            node_id: Some("scroll-3".into()),
            char_offset: "N3 row 000\n".chars().count() * 95,
        };
    }
    let mut a = Terminal::new(TestBackend::new(60, 28)).unwrap();
    let mut b = Terminal::new(TestBackend::new(60, 28)).unwrap();
    scroll_cells(&mut actual, &mut a);
    scroll_cells(&mut expected, &mut b);
    let actions = actual.event(key(KeyCode::PageDown));
    assert!(expected.event(key(KeyCode::PageDown)).is_empty());
    let (thread_id, generation, view_revision) = match &actions[0] {
        UiAction::LoadNodeWindow {
            thread_id,
            generation,
            view_revision,
            node_id,
            char_offset,
        } => {
            assert_eq!(node_id, "scroll-4");
            assert_eq!(*char_offset, 0);
            (thread_id.clone(), *generation, *view_revision)
        }
        other => panic!("{other:?}"),
    };
    actual.apply(UiUpdate::NodeWindow {
        thread_id,
        generation,
        view_revision,
        node: all[4].clone(),
    });
    assert!(!actual.view().loading);
    assert_eq!(
        scroll_cells(&mut actual, &mut a),
        scroll_cells(&mut expected, &mut b)
    );
    assert_eq!(actual.view().anchor, expected.view().anchor);
}
#[test]
fn zero_cache_navigation_converges_across_many_unknown_nodes_without_pin_growth() {
    let all = (0..10)
        .map(|i| {
            scroll_fixture_node(
                i,
                (0..8).map(|line| format!("N{i} row {line:03}\n")).collect(),
            )
        })
        .collect::<Vec<_>>();
    let mut config = Config::default();
    config.cache.body_bytes = 0;
    let mut actual = UiState::new(&config, cwd());
    actual.select_thread("scroll-fixture".into());
    actual.view_mut().follow = false;
    actual.view_mut().anchor = ViewAnchor {
        node_id: Some("scroll-0".into()),
        char_offset: 0,
    };
    let mut nodes = all.clone();
    for node in &mut nodes[1..] {
        node.text.clear();
    }
    actual.apply(UiUpdate::History {
        thread_id: "scroll-fixture".into(),
        generation: actual.view().generation,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 7,
        total_nodes: 10,
    });
    let mut expected = scroll_fixture(all.clone(), 0, 10);
    expected.view_mut().follow = false;
    expected.view_mut().anchor = ViewAnchor {
        node_id: Some("scroll-0".into()),
        char_offset: 0,
    };
    let mut a = Terminal::new(TestBackend::new(60, 28)).unwrap();
    let mut b = Terminal::new(TestBackend::new(60, 28)).unwrap();
    scroll_cells(&mut actual, &mut a);
    scroll_cells(&mut expected, &mut b);
    let mut requests = std::collections::VecDeque::new();
    for _ in 0..17 {
        requests.extend(scroll_wheel(&mut actual, false));
        assert!(scroll_wheel(&mut expected, false).is_empty());
    }
    let mut reads = 0;
    let mut body_reads = 0;
    while let Some(action) = requests.pop_front() {
        match action {
            UiAction::LoadNodeWindow {
                thread_id,
                generation,
                view_revision,
                node_id,
                ..
            } => {
                reads += 1;
                body_reads += 1;
                assert!(reads < 14, "known-row checkpoints must make progress");
                let node = all.iter().find(|n| n.node_id == node_id).unwrap().clone();
                actual.apply(UiUpdate::NodeWindow {
                    thread_id,
                    generation,
                    view_revision,
                    node,
                });
            }
            UiAction::LoadHistoryWindow {
                thread_id,
                generation,
                view_revision,
                ordinal,
                char_offset,
                from_end,
                selection_revision,
            } => {
                reads += 1;
                assert!(
                    reads < 14,
                    "metadata windows must advance with the working set"
                );
                let first = ordinal.saturating_sub(10) as usize;
                let nodes = all[first..(first + 50).min(all.len())].to_vec();
                let node = &all[ordinal as usize];
                actual.apply(UiUpdate::HistoryWindow {
                    thread_id,
                    generation,
                    view_revision,
                    nodes,
                    first_ordinal: first as u64,
                    selection_revision,
                    total_nodes: 10,
                    anchor: ViewAnchor {
                        node_id: Some(node.node_id.clone()),
                        char_offset: if from_end {
                            node.total_chars.saturating_sub(char_offset)
                        } else {
                            char_offset
                        },
                    },
                });
            }
            _ => continue,
        }
        assert!(
            actual.view().nodes().len() <= 3,
            "only the fixed viewport working set is exempt from the zero budget"
        );
        requests.extend(actual.take_deferred_actions());
    }
    assert!(body_reads >= 2);
    assert!(
        reads > body_reads,
        "exercise metadata as well as body loading"
    );
    assert!(!actual.view().loading);
    assert_eq!(actual.view().anchor, expected.view().anchor);
    assert_eq!(actual.view().anchor.node_id.as_deref(), Some("scroll-5"));
    assert_eq!(
        scroll_cells(&mut actual, &mut a).first(),
        scroll_cells(&mut expected, &mut b).first()
    );
}

#[test]
fn adjacent_partial_prefix_and_suffix_gaps_preserve_full_reference_page_positions() {
    let all = (0..3)
        .map(|i| {
            scroll_fixture_node(
                i,
                (0..100)
                    .map(|line| format!("N{i} row {line:03}\n"))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let row_chars = "N1 row 000\n".chars().count();
    for up in [true, false] {
        for tail in [true, false] {
            let mut nodes = all.clone();
            let first = if tail { 90 * row_chars } else { 0 };
            nodes[1].text = all[1]
                .text
                .chars()
                .skip(first)
                .take(10 * row_chars)
                .collect();
            nodes[1].base_char_offset = first;
            let mut actual = scroll_fixture(nodes, 0, 3);
            let mut expected = scroll_fixture(all.clone(), 0, 3);
            for ui in [&mut actual, &mut expected] {
                ui.view_mut().follow = false;
                ui.view_mut().anchor = ViewAnchor {
                    node_id: Some(if up { "scroll-2" } else { "scroll-0" }.into()),
                    char_offset: if up { 0 } else { 95 * row_chars },
                };
            }
            let mut a = Terminal::new(TestBackend::new(60, 28)).unwrap();
            let mut b = Terminal::new(TestBackend::new(60, 28)).unwrap();
            scroll_cells(&mut actual, &mut a);
            scroll_cells(&mut expected, &mut b);
            let code = if up {
                KeyCode::PageUp
            } else {
                KeyCode::PageDown
            };
            let actions = actual.event(key(code));
            assert!(expected.event(key(code)).is_empty());
            let (thread_id, generation, view_revision) = match &actions[0] {
                UiAction::LoadNodeWindow {
                    thread_id,
                    generation,
                    view_revision,
                    node_id,
                    ..
                } => {
                    assert_eq!(node_id, "scroll-1");
                    (thread_id.clone(), *generation, *view_revision)
                }
                other => panic!("{other:?}"),
            };
            actual.apply(UiUpdate::NodeWindow {
                thread_id,
                generation,
                view_revision,
                node: all[1].clone(),
            });
            assert!(!actual.view().loading, "up={up} tail={tail}");
            assert_eq!(
                scroll_cells(&mut actual, &mut a),
                scroll_cells(&mut expected, &mut b),
                "up={up} tail={tail}"
            );
            assert_eq!(
                actual.view().anchor,
                expected.view().anchor,
                "up={up} tail={tail}"
            );
        }
    }
}
#[test]
fn cancelling_an_inflight_scroll_allows_later_resize_and_rejects_its_stale_body() {
    use serde_json::json;
    use shellboy::presentation::{project, render_markdown};
    for switch_thread in [false, true] {
        let text = (0..80).map(|i| format!("row {i:03}\n")).collect::<String>();
        let full_source = format!("{}{text}", "p".repeat(90_000));
        let payload = json!({"output":[{"type":"message","content":[{"type":"output_text","text":full_source}]}]});
        let mut node = scroll_fixture_node(0, text.clone());
        node.base_char_offset = 90_000;
        node.total_chars = node.base_char_offset + text.chars().count();
        node.presentation = project("assistant_response", &payload, &text, node.base_char_offset);
        render_markdown(&mut node.presentation, 76);
        let mut ui = scroll_fixture(vec![node.clone()], 0, 1);
        let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
        scroll_cells(&mut ui, &mut terminal);
        let mut request = None;
        for _ in 0..10 {
            let actions = ui.event(key(KeyCode::PageUp));
            if let Some(UiAction::LoadNodeWindow {
                generation,
                view_revision,
                ..
            }) = actions.first()
            {
                request = Some((*generation, *view_revision));
                break;
            }
        }
        let (old_generation, old_revision) = request.expect("cross the bounded source window");
        assert!(ui.view().loading);
        if switch_thread {
            ui.select_thread("other".into());
            ui.select_thread("scroll-fixture".into());
        } else {
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::End,
                KeyModifiers::CONTROL,
            )));
        }
        let generation = ui.view().generation;
        ui.apply(UiUpdate::History {
            thread_id: "scroll-fixture".into(),
            generation,
            nodes: vec![node.clone()],
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 7,
            total_nodes: 1,
        });
        let mut stale = node.clone();
        stale.text = "STALE WINDOW".into();
        ui.apply(UiUpdate::NodeWindow {
            thread_id: "scroll-fixture".into(),
            generation: old_generation,
            view_revision: old_revision,
            node: stale,
        });
        assert_eq!(ui.view().nodes()[0].text, text);
        assert!(!ui.view().loading);
        ui.take_deferred_actions();
        terminal.backend_mut().resize(68, 28);
        scroll_cells(&mut ui, &mut terminal);
        assert!(
            ui.take_deferred_actions()
                .iter()
                .any(|action| matches!(action,
            UiAction::LoadNodeWindow{node_id,..} if node_id=="scroll-0")),
            "cancel must not permanently suppress Markdown reprojection: switch={switch_thread}"
        );
    }
}

#[test]
fn dense_thread_rows_and_selected_preview_stay_compact() {
    let mut ui = state();
    text(&mut ui, "/threads");
    enter(&mut ui);
    ui.apply(UiUpdate::Threads {
        generation: ui.list_generation(),
        requested_cursor: None,
        display_times: vec![],
        next_cursor: None,
        threads: (0..30)
            .map(|i| shellboy::protocol::ThreadSummary {
                thread_id: format!("thread-{i:02}"),
                title: format!("TITLE{i:02}"),
                cwd: "/synthetic/project".into(),
                parent_id: None,
                state: "idle".into(),
                updated_at: "2026-10-25T00:00:00Z".into(),
                excerpt: format!(
                    "# 見出し\n\n**本文** {} HIDDEN_TAIL",
                    "e\u{301}".repeat(100)
                ),
            })
            .collect(),
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let shown = screen_text(&terminal);
    assert!(shown.contains("見出し 本文"));
    assert!(
        !shown.contains("# 見出し")
            && !shown.contains("**本文**")
            && !shown.contains("HIDDEN_TAIL")
    );
    assert!(
        shown.contains("TITLE06") && shown.contains("入力できます"),
        "seven dense rows must preserve both application status rows: {shown}"
    );
    ui.event(key(KeyCode::End));
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let shown = screen_text(&terminal);
    assert!(
        shown.contains("TITLE29"),
        "selection must scroll into view: {shown}"
    );
    assert!(!shown.contains("TITLE00"));
    ui.event(key(KeyCode::F(1)));
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let details = screen_text(&terminal);
    assert!(details.contains("会話の情報"));
    assert!(details.contains("会話ID: thread-29"));
}

fn quiet_scroll_event(index: usize) -> UiNode {
    let mut node = scroll_fixture_node(index, "context changed".into());
    node.role = "event".into();
    node.presentation.kind = shellboy::presentation::PresentationKind::Event;
    node.presentation.title = "状態変更".into();
    node
}

#[test]
fn wheel_down_at_coalesced_event_tail_stays_at_the_true_end() {
    let nodes = vec![
        scroll_fixture_node(0, "user input".into()),
        quiet_scroll_event(1),
        quiet_scroll_event(2),
    ];
    let mut ui = scroll_fixture(nodes, 0, 3);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let before = scroll_cells(&mut ui, &mut terminal);
    assert!(before.iter().any(|line| line.contains("状態変更 2件")));
    for _ in 0..12 {
        assert!(scroll_wheel(&mut ui, false).is_empty());
        assert!(ui.view().follow);
        assert!(!ui.view().loading);
        assert_eq!(scroll_cells(&mut ui, &mut terminal), before);
    }
}

#[test]
fn wheel_crosses_large_coalesced_event_groups_without_recursive_calls_or_skipped_answer() {
    let mut nodes = vec![scroll_fixture_node(0, "FIRST USER".into())];
    nodes.extend((1..269).map(quiet_scroll_event));
    nodes.push(scroll_fixture_node(269, "FINAL ANSWER".into()));
    let mut ui = scroll_fixture(nodes, 0, 270);
    ui.view_mut().anchor = ViewAnchor {
        node_id: Some("scroll-0".into()),
        char_offset: 0,
    };
    ui.view_mut().follow = false;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    assert!(
        scroll_cells(&mut ui, &mut terminal)
            .iter()
            .any(|line| line.contains("FIRST USER"))
    );
    assert!(scroll_wheel(&mut ui, false).is_empty());
    assert!(ui.view().follow);
    assert!(
        scroll_cells(&mut ui, &mut terminal)
            .iter()
            .any(|line| line.contains("FINAL ANSWER"))
    );
    assert!(scroll_wheel(&mut ui, true).is_empty());
    assert!(
        scroll_cells(&mut ui, &mut terminal)
            .iter()
            .any(|line| line.contains("FIRST USER"))
    );
    assert!(!ui.view().loading);
}

fn recovery_node(id: &str) -> UiNode {
    let text = format!("visible history {id}");
    UiNode {
        node_id: id.into(),
        role: "assistant".into(),
        total_chars: text.chars().count(),
        text,
        base_char_offset: 0,
        reasoning_ranges: vec![],
        presentation: Default::default(),
    }
}
fn recovery_view() -> UiState {
    let mut ui = UiState::new(&Config::default(), cwd());
    ui.select_thread("recovery".into());
    ui.apply(UiUpdate::History {
        thread_id: "recovery".into(),
        generation: ui.view().generation,
        nodes: vec![recovery_node("old0"), recovery_node("old1")],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 2,
    });
    ui.view_mut().follow = false;
    ui.view_mut().anchor = ViewAnchor {
        node_id: Some("old1".into()),
        char_offset: 0,
    };
    ui.take_deferred_actions();
    ui
}
#[test]
fn expired_history_rebases_without_clearing_content_and_ignores_old_window_failures() {
    let mut ui = recovery_view();
    let generation = ui.view().generation;
    let previous = ui.view().view_revision;
    let anchor = ui.view().anchor.clone();
    ui.apply(UiUpdate::History {
        thread_id: "recovery".into(),
        generation,
        nodes: vec![recovery_node("old1"), recovery_node("new2")],
        before_cursor: None,
        append: false,
        first_ordinal: 1,
        selection_revision: 2,
        total_nodes: 3,
    });
    assert_eq!(
        ui.view().selection_revision,
        1,
        "new revision cannot be stamped onto old ordinals"
    );
    ui.apply(UiUpdate::HistoryInvalidated {
        thread_id: "recovery".into(),
        generation,
        view_revision: Some(previous),
    });
    let actions = ui.take_deferred_actions();
    assert!(matches!(&actions[..], [UiAction::RebaseHistory {anchor:point,..}] if point==&anchor));
    let current = ui.view().view_revision;
    assert_ne!(previous, current);
    assert!(!ui.view().nodes().is_empty());
    ui.apply(UiUpdate::LoadFailed {
        thread_id: "recovery".into(),
        generation,
        view_revision: Some(previous),
        message: "late old failure".into(),
    });
    assert!(ui.view().loading);
    ui.apply(UiUpdate::HistoryWindow {
        thread_id: "recovery".into(),
        generation,
        view_revision: previous,
        nodes: vec![recovery_node("stale")],
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 1,
        anchor: ViewAnchor::default(),
    });
    assert!(ui.view().nodes().iter().all(|n| n.node_id != "stale"));
    ui.apply(UiUpdate::HistoryWindow {
        thread_id: "recovery".into(),
        generation,
        view_revision: current,
        nodes: vec![
            recovery_node("old0"),
            recovery_node("old1"),
            recovery_node("new2"),
        ],
        first_ordinal: 0,
        selection_revision: 2,
        total_nodes: 3,
        anchor: anchor.clone(),
    });
    assert_eq!(ui.view().anchor, anchor);
    assert_eq!(ui.view().selection_revision, 2);
    assert!(!ui.view().loading);
    assert!(!ui.view().follow);
    assert!(ui.view().status.contains("再取得しました"));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(screen_text(&terminal).contains("visible history old1"));
}
#[test]
fn history_rebase_replaces_removed_anchor_and_rejects_old_refresh() {
    let mut ui = recovery_view();
    let generation = ui.view().generation;
    ui.apply(UiUpdate::HistoryInvalidated {
        thread_id: "recovery".into(),
        generation,
        view_revision: Some(ui.view().view_revision),
    });
    let current = ui.view().view_revision;
    ui.take_deferred_actions();
    ui.apply(UiUpdate::HistoryWindow {
        thread_id: "recovery".into(),
        generation,
        view_revision: current,
        nodes: vec![recovery_node("replacement")],
        first_ordinal: 0,
        selection_revision: 5,
        total_nodes: 1,
        anchor: ViewAnchor {
            node_id: Some("replacement".into()),
            char_offset: 0,
        },
    });
    assert_eq!(ui.view().nodes()[0].node_id, "replacement");
    assert_eq!(ui.view().anchor.node_id.as_deref(), Some("replacement"));
    assert!(ui.view().status.contains("選択経路から外れた"));
    ui.apply(UiUpdate::History {
        thread_id: "recovery".into(),
        generation,
        nodes: vec![recovery_node("old0")],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 2,
    });
    assert_eq!(ui.view().nodes()[0].node_id, "replacement");
    assert_eq!(ui.view().total_nodes, 1);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(screen_text(&terminal).contains("visible history replacement"));
}
#[test]
fn follow_latest_cancels_an_outstanding_history_rebase() {
    let mut ui = recovery_view();
    let generation = ui.view().generation;
    ui.apply(UiUpdate::HistoryInvalidated {
        thread_id: "recovery".into(),
        generation,
        view_revision: Some(ui.view().view_revision),
    });
    let previous = ui.view().view_revision;
    ui.take_deferred_actions();
    ui.event(Event::Key(KeyEvent::new(
        KeyCode::End,
        KeyModifiers::CONTROL,
    )));
    assert!(ui.view().follow);
    assert_ne!(ui.view().view_revision, previous);
    ui.apply(UiUpdate::HistoryWindow {
        thread_id: "recovery".into(),
        generation,
        view_revision: previous,
        nodes: vec![recovery_node("late")],
        first_ordinal: 0,
        selection_revision: 3,
        total_nodes: 1,
        anchor: ViewAnchor::default(),
    });
    assert!(ui.view().follow);
    assert!(ui.view().nodes().iter().all(|node| node.node_id != "late"));
}

#[test]
fn history_rebase_adopts_clamped_character_offset_for_a_surviving_node() {
    let mut ui = recovery_view();
    let generation = ui.view().generation;
    ui.view_mut().anchor.char_offset = 900;
    ui.apply(UiUpdate::HistoryInvalidated {
        thread_id: "recovery".into(),
        generation,
        view_revision: Some(ui.view().view_revision),
    });
    let current = ui.view().view_revision;
    ui.take_deferred_actions();
    let node = recovery_node("old1");
    let clamped = node.total_chars;
    ui.apply(UiUpdate::HistoryWindow {
        thread_id: "recovery".into(),
        generation,
        view_revision: current,
        nodes: vec![node],
        first_ordinal: 0,
        selection_revision: 2,
        total_nodes: 1,
        anchor: ViewAnchor {
            node_id: Some("old1".into()),
            char_offset: clamped,
        },
    });
    assert_eq!(ui.view().anchor.char_offset, clamped);
    assert!(!ui.view().loading);
}

#[test]
fn sending_cancels_history_rebase_and_later_navigation_can_recover_again() {
    let mut ui = recovery_view();
    let generation = ui.view().generation;
    ui.apply(UiUpdate::HistoryInvalidated {
        thread_id: "recovery".into(),
        generation,
        view_revision: Some(ui.view().view_revision),
    });
    let previous = ui.view().view_revision;
    ui.take_deferred_actions();
    ui.view_mut().draft.text = "new request".into();
    ui.view_mut().draft.cursor = "new request".len();
    let actions = ui.event(key(KeyCode::Enter));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, UiAction::Send { .. }))
    );
    assert!(ui.view().follow);
    ui.apply(UiUpdate::HistoryWindow {
        thread_id: "recovery".into(),
        generation,
        view_revision: previous,
        nodes: vec![recovery_node("late")],
        first_ordinal: 0,
        selection_revision: 2,
        total_nodes: 1,
        anchor: ViewAnchor::default(),
    });
    assert!(ui.view().follow);
    ui.view_mut().follow = false;
    ui.apply(UiUpdate::HistoryInvalidated {
        thread_id: "recovery".into(),
        generation,
        view_revision: Some(ui.view().view_revision),
    });
    assert!(
        ui.take_deferred_actions()
            .iter()
            .any(|action| matches!(action, UiAction::RebaseHistory { .. }))
    );
}

#[test]
fn rejected_quit_restores_editing_and_can_be_retried() {
    let mut ui = state();
    ui.insert("retained draft", false);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    sender.try_send(UiAction::Quit).unwrap();
    let attempts = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL,
    )));
    assert!(ui.is_quitting());
    assert!(
        attempts
            .iter()
            .any(|action| matches!(action, UiAction::Quit))
    );
    for action in attempts {
        let error = sender.try_send(action).unwrap_err();
        ui.action_rejected(&error.into_inner());
    }
    assert!(
        !ui.is_quitting(),
        "rejected shutdown must not trap the UI in shutdown mode"
    );
    assert_eq!(ui.view().draft.text, "retained draft");
    ui.event(key(KeyCode::Char('x')));
    assert_eq!(ui.view().draft.text, "retained draftx");
    receiver.try_recv().unwrap();
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL,
    )));
    let quit = actions
        .into_iter()
        .find(|action| matches!(action, UiAction::Quit))
        .unwrap();
    sender.try_send(quit).unwrap();
    assert!(ui.is_quitting());
    assert!(matches!(receiver.try_recv().unwrap(), UiAction::Quit));
}

#[test]
fn rejected_slash_quit_recovers_but_cannot_undo_acknowledged_shutdown() {
    let mut ui = state();
    ui.insert("/quit", false);
    let actions = enter(&mut ui);
    let quit = actions
        .iter()
        .find(|action| matches!(action, UiAction::Quit))
        .unwrap();
    assert!(ui.is_quitting());
    ui.action_rejected(quit);
    assert!(!ui.is_quitting());
    ui.apply(UiUpdate::ShutdownProgress {
        elapsed_ms: 0,
        detail: "stopping".into(),
        force_available: false,
    });
    ui.action_rejected(quit);
    assert!(ui.is_quitting());
    assert!(ui.event(key(KeyCode::Char('x'))).is_empty());
    assert!(ui.view().draft.text.is_empty());
}

#[test]
fn editing_while_accepting_keeps_text_attachments_and_counts_the_full_byte_limit() {
    let mut config = Config::default();
    config.limits.draft_bytes = 11;
    let mut ui = UiState::new(&config, cwd());
    ui.insert("日本", false);
    ui.apply(UiUpdate::ClipboardAttachment {
        draft_id: ui.view().draft.draft_id.clone(),
        revision: ui.view().draft.revision,
        attachment: UiAttachment {
            id: "image".into(),
            path: cwd().join("image.png"),
            kind: "image".into(),
            state: "ready".into(),
            position: None,
        },
    });
    let actions = enter(&mut ui);
    let (draft_id, revision) = sent(&actions);
    let attachments = ui.view().draft.attachments.clone();
    ui.insert("abc", false);
    assert_eq!(ui.view().draft.text, "日本\u{fffc}");
    assert_eq!(ui.view().draft.attachments, attachments);
    ui.insert("!", false);
    assert_eq!(ui.view().draft.text, "日本\u{fffc}!");
    assert_eq!(ui.view().draft.attachments, attachments);
    ui.apply(UiUpdate::Receipt {
        draft_id,
        revision,
        thread_id: None,
        status: ReceiptStatus::Failed,
        message: Some("not accepted".into()),
    });
    assert_eq!(ui.view().draft.text, "日本\u{fffc}!");
    assert!(ui.view().status.contains(&format!("/restore {revision}")));
    assert!(!ui.view().status.contains("<revision>"));
    assert_eq!(ui.view().pending[&revision].snapshot.text, "日本\u{fffc}");
}

#[test]
fn slash_rejection_restores_the_complete_input_without_relying_on_status_text() {
    for command in [
        "/compact",
        "/fork node",
        "/cwd /missing",
        "/background",
        "/edit user",
        "/copy node",
    ] {
        let mut ui = state();
        ui.select_thread("saved".into());
        ui.insert(command, false);
        ui.apply(UiUpdate::ClipboardAttachment {
            draft_id: ui.view().draft.draft_id.clone(),
            revision: ui.view().draft.revision,
            attachment: UiAttachment {
                id: "attachment".into(),
                path: cwd().join("not-read.png"),
                kind: "image".into(),
                state: "ready".into(),
                position: None,
            },
        });
        ui.event(key(KeyCode::Left));
        let original = ui.view().draft.clone();
        let actions = enter(&mut ui);
        assert!(
            matches!(actions[0], UiAction::SubmittedCommand { .. }),
            "{command}"
        );
        assert_eq!(ui.view().draft.text, "\u{fffc}");
        let mut retained = original.attachments.clone();
        retained[0].position = Some(0);
        assert_eq!(ui.view().draft.attachments, retained);
        let (sender, _receiver) = actions::channel(1);
        sender
            .try_send(UiAction::RenderWidth { width: 80 })
            .unwrap();
        let rejected = sender
            .try_send(actions[0].clone())
            .unwrap_err()
            .into_inner();
        ui.action_rejected(&rejected);
        let restored = &ui.view().draft;
        assert_eq!(restored.text, original.text, "{command}");
        assert_eq!(restored.cursor, original.cursor, "{command}");
        assert_eq!(restored.attachments, original.attachments, "{command}");
        assert!(restored.revision > original.revision);
        assert!(ui.view().status.contains("復元"));
    }
    let mut ui = state();
    ui.insert("/does-not-exist 日本", false);
    let original = ui.view().draft.clone();
    assert!(enter(&mut ui).is_empty());
    assert_eq!(ui.view().draft, original);
}

#[test]
fn late_slash_failure_preserves_new_text_and_clipboard_and_targets_its_own_view() {
    let mut ui = state();
    ui.select_thread("source".into());
    ui.insert("/compact", false);
    let original = ui.view().draft.clone();
    let actions = enter(&mut ui);
    let UiAction::SubmittedCommand { submission, .. } = &actions[0] else {
        panic!()
    };
    let submission = submission.clone();
    // A clipboard reply received during the operation becomes a newer draft.
    let cleared = ui.view().draft.clone();
    ui.apply(UiUpdate::ClipboardAttachment {
        draft_id: cleared.draft_id,
        revision: cleared.revision,
        attachment: UiAttachment {
            id: "new-image".into(),
            path: cwd().join("not-read.png"),
            kind: "image".into(),
            state: "ready".into(),
            position: None,
        },
    });
    let with_image = ui.view().draft.clone();
    ui.apply(UiUpdate::CommandOutcome {
        submission: submission.clone(),
        result: Err("rejected".into()),
    });
    assert_eq!(ui.view().draft, with_image);
    assert!(
        ui.view()
            .status
            .contains(&format!("/restore {}", original.revision))
    );
    // A separate invocation fails for the first time after typing and a switch.
    let mut ui = state();
    ui.select_thread("source".into());
    ui.insert("/compact", false);
    let actions = enter(&mut ui);
    let UiAction::SubmittedCommand { submission, .. } = &actions[0] else {
        panic!()
    };
    let submission = submission.clone();
    ui.insert("new input", false);
    let newer = ui.view().draft.clone();
    ui.select_thread("other".into());
    ui.insert("other input", false);
    let other = ui.view().draft.clone();
    ui.apply(UiUpdate::CommandOutcome {
        submission,
        result: Err("late first failure".into()),
    });
    assert_eq!(ui.view().draft, other);
    assert_eq!(
        ui.views
            .values()
            .find(|view| view.thread_id.as_deref() == Some("source"))
            .unwrap()
            .draft,
        newer
    );
}

#[test]
fn successful_slash_ack_releases_recovery_and_late_duplicate_cannot_restore_it() {
    let mut ui = state();
    ui.select_thread("saved".into());
    ui.insert("/compact", false);
    let actions = enter(&mut ui);
    let UiAction::SubmittedCommand { submission, .. } = &actions[0] else {
        panic!()
    };
    let submission = submission.clone();
    ui.insert("new input", false);
    let newer = ui.view().draft.clone();
    ui.apply(UiUpdate::CommandOutcome {
        submission: submission.clone(),
        result: Ok(()),
    });
    assert!(!ui.view().pending.contains_key(&submission.revision));
    ui.apply(UiUpdate::CommandOutcome {
        submission,
        result: Err("late".into()),
    });
    assert_eq!(ui.view().draft, newer);
}

#[test]
fn slash_quit_persists_the_cleared_command_and_cancel_retains_input_on_coalesced_controls() {
    let mut ui = state();
    ui.select_thread("saved".into());
    ui.insert("/quit", false);
    let previous = ui.view().draft.revision;
    let actions = enter(&mut ui);
    assert!(
        matches!(&actions[..], [UiAction::SaveDraft { draft, .. }, UiAction::Quit] if draft.text.is_empty() && draft.revision > previous)
    );
    let mut ui = state();
    ui.select_thread("saved".into());
    ui.insert("/cancel", false);
    let original = ui.view().draft.clone();
    let actions = enter(&mut ui);
    assert!(matches!(&actions[..], [UiAction::Cancel { .. }]));
    ui.action_rejected(&actions[0]);
    assert_eq!(ui.view().draft, original);
}

#[test]
fn completion_prefers_exact_commands_and_keeps_prefix_expansion_nonexecuting() {
    for (command, cancel) in [("/cancel", true), ("/cancel-wait", false)] {
        let mut ui = state();
        ui.select_thread("saved".into());
        ui.insert(command, false);
        let actions = enter(&mut ui);
        if cancel {
            assert!(
                matches!(&actions[..], [UiAction::Cancel { thread_id }] if thread_id == "saved")
            );
        } else {
            assert!(matches!(&actions[..], [UiAction::CancelWait]));
        }
        assert_eq!(ui.view().draft.text, command);
    }
    for code in [KeyCode::Tab, KeyCode::Enter] {
        let mut ui = state();
        ui.insert("/can", false);
        assert!(ui.event(key(code)).is_empty());
        assert_eq!(ui.view().draft.text, "/cancel-wait ");
    }
    let mut ui = state();
    ui.insert("/cancel", false);
    assert!(ui.event(key(KeyCode::Tab)).is_empty());
    assert_eq!(ui.view().draft.text, "/cancel ");
    let mut ui = state();
    ui.insert("/cancel", false);
    // The user may deliberately choose the longer command instead.
    assert!(ui.event(key(KeyCode::Down)).is_empty());
    assert!(enter(&mut ui).is_empty());
    assert_eq!(ui.view().draft.text, "/cancel-wait ");
}

#[test]
fn saved_thread_recovery_status_shows_message_without_serialized_error_fields() {
    for (reason, expected) in [
        (
            r#"{"code":"io_failed","message":"保存済みの要求は再試行できます","details":{"private_fixture":"raw-only"}}"#,
            "保存済みの要求は再試行できます",
        ),
        (
            r#"{"unknown":"legacy-fixture-raw-only"}"#,
            "保存された復旧理由を詳細で確認してください",
        ),
    ] {
        let mut ui = state();
        ui.select_thread("recovery-thread".into());
        let details = serde_json::from_value(serde_json::json!({
            "thread_id":"recovery-thread", "title":"復旧確認", "cwd":"/tmp",
            "connection":"fixture", "model":"fixture", "run_id":"saved-run",
            "turn_id":"saved-turn", "children_count":0, "shells_count":0,
            "recovery_reason":reason
        }))
        .unwrap();
        ui.apply(UiUpdate::Thread {
            generation: ui.view().generation,
            details,
            draft: None,
        });
        assert_eq!(ui.view().status, format!("復旧待ち: {expected}"));
        assert_eq!(ui.view().run_id.as_deref(), Some("saved-run"));
        for width in [60, 110] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
            let screen = screen_text(&terminal);
            assert!(!screen.contains("io_failed") && !screen.contains("raw-only"));
        }
    }
}

#[test]
fn recovery_reason_survives_transient_results_and_retry_targets_the_current_run() {
    use shellboy::presentation::PresentationField;
    use shellboy::ui::activity::{ActivityPhase, ActivitySnapshot, RecoverySnapshot};
    let mut ui = state();
    ui.select_thread("current-thread".into());
    ui.view_mut().run_id = Some("stale-view-run".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::Activity {
        thread_id: "current-thread".into(),
        generation,
        epoch: 1,
        observed_at: std::time::Instant::now(),
        snapshot: ActivitySnapshot {
            run_id: Some("current-recovery-run".into()),
            turn_id: Some("current-turn".into()),
            phase: ActivityPhase::Recovery,
            recovery: Some(RecoverySnapshot {
                stage: "モデル要求".into(),
                message: "fixture failure".into(),
                pending_messages: 1,
                saved_tool_results: 2,
                ..Default::default()
            }),
            ..Default::default()
        },
    });
    ui.apply(UiUpdate::StructuredManagement {
        work_status: false,
        request_id: 1,
        title: "古い結果".into(),
        omitted: false,
        fields: vec![PresentationField {
            value_omitted: false,
            display: None,
            key: "/run_id".into(),
            label: "Run ID".into(),
            value: "past-reference-run".into(),
        }],
    });
    ui.event(key(KeyCode::Esc));
    ui.apply(UiUpdate::Status {
        thread_id: None,
        text: "別操作の結果を取得しました".into(),
    });
    for (width, height) in [(110, 28), (60, 20)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let screen = screen_text(&terminal);
        assert!(screen.contains("復旧待ち"));
        assert!(screen.contains("fixture failure"));
        assert!(screen.contains("/retry"));
        assert!(screen.contains("操作を待っています"));
        assert!(!screen.contains("経過"));
        assert!(!screen.contains("tool 0") && !screen.contains("取消 0"));
    }
    ui.insert("/retry", false);
    let actions = enter(&mut ui);
    assert!(
        matches!(command_action(&actions[0]),UiAction::Command(shellboy::protocol::Command::RunRetry(target)) if target.run_id == "current-recovery-run")
    );
    ui.action_rejected(&actions[0]);
    assert_eq!(ui.view().draft.text, "/retry");
}

#[test]
fn stopped_recovery_keeps_the_original_reason_and_requires_a_new_explicit_choice() {
    use shellboy::ui::activity::{ActivityPhase, ActivitySnapshot, RecoverySnapshot};
    let mut ui = state();
    ui.select_thread("thread".into());
    ui.apply(UiUpdate::Activity {
        thread_id: "thread".into(),
        generation: ui.view().generation,
        epoch: 1,
        observed_at: std::time::Instant::now(),
        snapshot: ActivitySnapshot {
            run_id: Some("stopped-run".into()),
            phase: ActivityPhase::Stopped,
            recovery: Some(RecoverySnapshot {
                stage: "処理（段階の記録なし）".into(),
                message: "original failure".into(),
                stop_reason: Some("explicit_stop".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
    });
    let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("前回の復旧待ち")
            && screen.contains("人による停止")
            && screen.contains("original failure")
    );
    ui.insert("/retry", false);
    assert!(enter(&mut ui).is_empty());
    assert_eq!(ui.view().draft.text, "/retry");
}

#[test]
fn tab_in_management_detail_does_not_move_the_hidden_field_selection() {
    use shellboy::presentation::PresentationField;
    let mut ui = state();
    ui.apply(UiUpdate::StructuredManagement {
        work_status: false,
        request_id: 7,
        title: "fixture".into(),
        omitted: false,
        fields: vec![
            PresentationField {
                value_omitted: false,
                display: None,
                key: "/first".into(),
                label: "first".into(),
                value: "one".into(),
            },
            PresentationField {
                value_omitted: false,
                display: None,
                key: "/second".into(),
                label: "second".into(),
                value: "two".into(),
            },
        ],
    });
    assert!(
        matches!(&command_actions(&enter(&mut ui))[..],[UiAction::LoadManagementDetail {pointer,..}] if pointer=="/first")
    );
    ui.apply(UiUpdate::ManagementDetail {
        request_id: 7,
        pointer: "/first".into(),
        text: "full one".into(),
        next_cursor: None,
    });
    assert!(ui.event(key(KeyCode::Tab)).is_empty());
    ui.event(key(KeyCode::Esc));
    assert!(
        matches!(&command_actions(&enter(&mut ui))[..],[UiAction::LoadManagementDetail {pointer,..}] if pointer=="/first")
    );
}

#[test]
fn a_literal_omission_marker_is_copied_as_complete_metadata() {
    let mut ui = state();
    let literal = "利用者が入力した [詳細は原文参照] という文字列";
    let fields = shellboy::presentation::fields_of_value(&serde_json::json!({"note":literal}));
    ui.apply(UiUpdate::StructuredManagement {
        work_status: false,
        request_id: 391,
        title: "完全な値".into(),
        fields,
        omitted: false,
    });
    assert!(
        matches!(&ui.event(key(KeyCode::Char('c')))[..], [UiAction::CopyText { text, scope: shellboy::ui::CopyScope::Value }] if text == literal),
        "a literal marker must not start a full-value request"
    );
}

#[test]
fn omitted_long_and_deep_metadata_use_the_complete_value_reader_for_copy() {
    let mut deep = serde_json::json!("deep original");
    for _ in 0..12 {
        deep = serde_json::json!({"child":deep});
    }
    for value in [serde_json::json!({"note":"あ".repeat(1025)}), deep] {
        let mut ui = state();
        let fields = shellboy::presentation::fields_of_value(&value);
        assert_eq!(fields.len(), 1);
        assert!(fields[0].value_omitted);
        let pointer = fields[0].key.clone();
        ui.apply(UiUpdate::StructuredManagement {
            work_status: false,
            request_id: 392,
            title: "省略した値".into(),
            fields,
            omitted: true,
        });
        assert!(
            matches!(&command_actions(&ui.event(key(KeyCode::Char('c'))))[..],
            [UiAction::LoadManagementDetail { request_id:392, pointer:actual, cursor:None }] if actual == &pointer)
        );
        ui.apply(UiUpdate::ManagementDetail {
            request_id: 392,
            pointer,
            text: "完全な原文の値".into(),
            next_cursor: None,
        });
        assert!(matches!(&ui.event(key(KeyCode::Char('c')))[..],
            [UiAction::CopyText { text, scope: shellboy::ui::CopyScope::ManagementPage { has_more: false } }] if text == "完全な原文の値"));
    }
}

fn shellboy_provenance_fixture() -> shellboy::protocol::NodeProvenance {
    shellboy::protocol::NodeProvenance {
        kind: "assistant_response".into(),
        created_at: "2026-10-25T00:00:00Z".into(),
        source_revision: 0,
        turn_id: None,
        run_id: None,
        origin_thread_id: None,
    }
}
