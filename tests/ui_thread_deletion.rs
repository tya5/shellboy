use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};
use shellboy::{
    config::Config,
    protocol::{ApiError, ErrorCode, ThreadDeleteArgs, ThreadSummary},
    ui::{ThreadDeletionReply, UiAction, UiState, UiUpdate, draw},
};
use unicode_width::UnicodeWidthStr;

fn key(ui: &mut UiState, code: KeyCode) -> Vec<UiAction> {
    ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}
fn list(ui: &mut UiState, ids: &[&str]) {
    if !ui.threads_visible() {
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        )));
    }
    ui.apply(UiUpdate::Threads {
        read_sequence: 0,
        generation: ui.list_generation(),
        requested_cursor: None,
        threads: ids
            .iter()
            .enumerate()
            .map(|(position, id)| ThreadSummary {
                last_answer: None,
                last_input: None,
                activity: shellboy::activity::ActivityPhase::Idle,
                latest_run_stop_reason: None,
                revisit: None,
                thread_id: (*id).into(),
                title: format!("title {id}"),
                cwd: "/synthetic".into(),
                parent_id: None,
                state: "completed".into(),
                // Supplied fixture order represents a newest-first server page.
                updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(
                    1_700_000_000 - position as i64,
                    0,
                )
                .unwrap()
                .to_rfc3339(),
                excerpt: String::new(),
            })
            .collect(),
        display_times: vec![],
        next_cursor: None,
    });
}
fn request(actions: Vec<UiAction>, execute_expected: bool) -> (u64, ThreadDeleteArgs) {
    let action = actions
        .into_iter()
        .find(|action| matches!(action, UiAction::ThreadDeletion { .. }))
        .expect("deletion action");
    let UiAction::ThreadDeletion {
        request_id,
        args,
        execute,
        ..
    } = action
    else {
        unreachable!()
    };
    assert_eq!(execute, execute_expected);
    (request_id, args)
}
fn preview(args: &ThreadDeleteArgs, deletable: bool) -> Value {
    json!({"thread_id":args.thread_id,"descendants":args.descendants,"deletable_now":deletable,
        "selection":{"thread_count":if args.descendants {2}else{1},"descendant_thread_count":1},
        "blockers":{"live_run_count":if deletable {0}else{1},"stopping_run_count":0,"runs":if deletable {json!([])}else{json!([{"id":"blocking-run","state":"live"}])}},
        "references":{"shared_path_nodes":2,"completed_replacement_source_nodes":3,"attachment_reference_count":4}})
}
fn accept(ui: &mut UiState, id: u64, args: &ThreadDeleteArgs, deletable: bool) {
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: id,
        args: args.clone(),
        execute: false,
        result: Ok(ThreadDeletionReply::Preview(preview(args, deletable))),
    });
}
fn screen(ui: &mut UiState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(110, 45)).unwrap();
    terminal.draw(|frame| draw(frame, ui)).unwrap();
    let buffer = terminal.backend().buffer();
    (buffer.area.y..buffer.area.bottom())
        .map(|y| {
            let mut line = String::new();
            let mut x = buffer.area.x;
            while x < buffer.area.right() {
                let symbol = buffer[(x, y)].symbol();
                line.push_str(symbol);
                x = x.saturating_add(symbol.width().max(1) as u16);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn ui() -> UiState {
    UiState::new(&Config::default(), std::env::temp_dir())
}

#[test]
fn list_delete_waits_for_the_exact_preview_and_supports_cancel() {
    let mut ui = ui();
    list(&mut ui, &["chosen", "next"]);
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    assert!(!args.descendants);
    assert!(key(&mut ui, KeyCode::Char('y')).is_empty());
    let mut wrong = args.clone();
    wrong.thread_id = "different".into();
    accept(&mut ui, id, &wrong, true);
    assert!(key(&mut ui, KeyCode::Char('y')).is_empty());
    accept(&mut ui, id, &args, true);
    let shown = screen(&mut ui);
    assert!(shown.contains("会話の削除確認") && shown.contains("削除できます"));
    assert!(shown.contains(&format!("対象: title {}", args.thread_id)));
    assert!(shown.contains(&format!("会話ID: {}", args.thread_id)));
    assert!(
        shown.contains("共有されている履歴: 2")
            && shown.contains("置換の出典: 3")
            && shown.contains("添付参照: 4")
    );
    assert!(key(&mut ui, KeyCode::Char('n')).is_empty());
    assert!(ui.threads_visible());
    assert_eq!(ui.threads().len(), 2);
    assert!(!screen(&mut ui).contains("会話の削除確認"));
}

#[test]
fn descendants_toggle_requires_a_fresh_matching_preview_and_live_runs_block_y() {
    let mut ui = ui();
    list(&mut ui, &["chosen"]);
    let (old, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, old, &args, true);
    let (fresh, with_children) = request(key(&mut ui, KeyCode::Char(' ')), false);
    assert!(with_children.descendants);
    assert_ne!(fresh, old);
    accept(&mut ui, old, &args, true);
    assert!(key(&mut ui, KeyCode::Char('y')).is_empty());
    accept(&mut ui, fresh, &with_children, false);
    assert!(screen(&mut ui).contains("blocking-run"));
    assert!(key(&mut ui, KeyCode::Char('Y')).is_empty());
    let (again, args) = request(key(&mut ui, KeyCode::Char('d')), false);
    assert!(!args.descendants);
    accept(&mut ui, again, &args, true);
    let (_, execute) = request(key(&mut ui, KeyCode::Char('Y')), true);
    assert!(!execute.descendants);
    assert!(
        key(&mut ui, KeyCode::Char('y')).is_empty(),
        "execution cannot be duplicated"
    );
}

#[test]
fn background_success_updates_the_list_and_invalidates_old_pages_without_switching_the_view() {
    let mut ui = ui();
    list(&mut ui, &["chosen", "next"]);
    let active = ui.active.clone();
    let old = ui.list_generation();
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, id, &args, true);
    let (id, args) = request(key(&mut ui, KeyCode::Char('y')), true);
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: id,
        args: args.clone(),
        execute: true,
        result: Ok(ThreadDeletionReply::Deleted(vec![args.thread_id.clone()])),
    });
    assert!(ui.threads_visible());
    assert_eq!(ui.active, active);
    assert_eq!(ui.threads().len(), 1);
    assert_eq!(ui.thread_selection(), 0);
    ui.apply(UiUpdate::Threads {
        read_sequence: 0,
        generation: old,
        requested_cursor: None,
        threads: vec![ThreadSummary {
            last_answer: None,
            last_input: None,
            activity: shellboy::activity::ActivityPhase::Idle,
            latest_run_stop_reason: None,
            revisit: None,
            thread_id: args.thread_id,
            title: "stale deleted row".into(),
            cwd: "/".into(),
            parent_id: None,
            state: "completed".into(),
            updated_at: "now".into(),
            excerpt: String::new(),
        }],
        display_times: vec![],
        next_cursor: None,
    });
    assert_eq!(ui.threads().len(), 1);
    assert_ne!(ui.threads()[0].title, "stale deleted row");
}

#[test]
fn accepted_deletion_cleans_active_and_descendant_views_even_after_the_dialog_is_closed() {
    let mut ui = ui();
    ui.select_thread("unrelated".into());
    ui.select_thread("child".into());
    ui.select_thread("parent".into());
    ui.insert("deleted thread draft", false);
    let deleted_draft = ui.view().draft.draft_id.clone();
    list(&mut ui, &["parent"]);
    request(key(&mut ui, KeyCode::Delete), false);
    let (id, args) = request(key(&mut ui, KeyCode::Char('d')), false);
    accept(&mut ui, id, &args, true);
    let (id, args) = request(key(&mut ui, KeyCode::Char('y')), true);
    key(&mut ui, KeyCode::Esc);
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: id,
        args,
        execute: true,
        result: Ok(ThreadDeletionReply::Deleted(vec![
            "parent".into(),
            "child".into(),
        ])),
    });
    assert!(ui.view().thread_id.is_none());
    assert_ne!(ui.view().draft.draft_id, deleted_draft);
    assert!(ui.view().draft.text.is_empty());
    assert!(!ui.threads_visible());
    assert!(
        ui.views
            .values()
            .all(|view| !matches!(view.thread_id.as_deref(), Some("parent" | "child")))
    );
    assert!(
        ui.views
            .values()
            .any(|view| view.thread_id.as_deref() == Some("unrelated"))
    );
    assert!(ui.take_deferred_actions().iter().all(|action|!matches!(action,UiAction::SaveDraft {thread_id,..} if thread_id=="parent"||thread_id=="child")&&!matches!(action,UiAction::DraftAttachments {draft_id,..} if draft_id==&deleted_draft)));
    ui.insert("new editable draft", false);
    assert_eq!(ui.view().draft.text, "new editable draft");
}

#[test]
fn execution_failure_is_visible_and_keeps_the_thread_and_draft() {
    let mut ui = ui();
    list(&mut ui, &["chosen"]);
    let draft = ui.view().draft.clone();
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, id, &args, true);
    let (id, args) = request(key(&mut ui, KeyCode::Char('y')), true);
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: id,
        args,
        execute: true,
        result: Err(ApiError::new(
            ErrorCode::InvalidState,
            "作業が開始されました",
        )),
    });
    assert_eq!(ui.threads().len(), 1);
    assert_eq!(ui.view().draft, draft);
    assert!(screen(&mut ui).contains("作業が開始されました"));
    assert!(key(&mut ui, KeyCode::Char('y')).is_empty());
    key(&mut ui, KeyCode::Esc);
    assert!(screen(&mut ui).contains("削除できません"));
}

#[test]
fn rejected_preview_or_execution_queue_admission_cannot_leave_an_enabled_delete() {
    let mut ui = ui();
    list(&mut ui, &["chosen"]);
    let actions = key(&mut ui, KeyCode::Delete);
    ui.action_rejected(&actions[0]);
    assert!(key(&mut ui, KeyCode::Char('y')).is_empty());
    key(&mut ui, KeyCode::Esc);
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, id, &args, true);
    let actions = key(&mut ui, KeyCode::Char('y'));
    ui.action_rejected(&actions[0]);
    assert!(key(&mut ui, KeyCode::Char('y')).is_empty());
    assert_eq!(ui.threads().len(), 1);
}

#[test]
fn images_in_an_edit_fork_draft_are_owned_by_the_source_thread_for_deletion() {
    let mut ui = ui();
    ui.select_thread("source".into());
    let generation = ui.view().generation;
    let revision = ui.view().draft.revision;
    ui.apply(UiUpdate::EditRequestLoaded {
        thread_id: "source".into(),
        node_id: "request".into(),
        generation,
        draft_revision: revision,
        text: "edited request".into(),
        attachments: vec![],
        run_id: None,
    });
    assert!(ui.view().thread_id.is_none());
    let draft_id = ui.view().draft.draft_id.clone();
    // Slash commands are submitted through the same Enter path as the TUI.
    let active = ui.active.clone();
    let draft = &mut ui.views.get_mut(&active).unwrap().draft;
    draft.text = "/image".into();
    draft.cursor = draft.text.len();
    draft.literal_paste = false;
    let actions = key(&mut ui, KeyCode::Enter);
    fn capture_owner(action: &UiAction) -> Option<&str> {
        match action {
            UiAction::PasteImage { thread_id, .. } => thread_id.as_deref(),
            UiAction::SubmittedCommand { action, .. } => capture_owner(action),
            _ => None,
        }
    }
    assert!(
        actions
            .iter()
            .any(|action| capture_owner(action) == Some("source"))
    );
    // A deletion completed from another view must also discard this editor.
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: 0,
        args: ThreadDeleteArgs {
            thread_id: "source".into(),
            descendants: false,
        },
        execute: true,
        result: Ok(ThreadDeletionReply::Deleted(vec!["source".into()])),
    });
    assert_ne!(ui.view().draft.draft_id, draft_id);
    assert!(ui.view().draft.edit.is_none());
}

#[test]
fn cleanup_warning_preserves_applied_deletion_and_moves_current_to_editable_draft() {
    let mut ui = ui();
    ui.select_thread("chosen".into());
    list(&mut ui, &["chosen"]);
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, id, &args, true);
    let (id, args) = request(key(&mut ui, KeyCode::Char('y')), true);
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: id,
        args,
        execute: true,
        result: Ok(ThreadDeletionReply::DeletedWithWarning {
            ids: vec!["chosen".into()],
            warning: "fixture saved-view cleanup failure".into(),
        }),
    });
    assert!(ui.view().thread_id.is_none());
    assert!(!ui.threads_visible());
    assert!(
        ui.view()
            .status
            .contains("削除しました。後片付けに失敗しました")
    );
    assert!(!ui.view().status.contains("削除できません"));
    ui.insert("fresh", false);
    assert_eq!(ui.view().draft.text, "fresh");
}

#[test]
fn closing_and_reopening_cannot_submit_a_second_delete_of_the_same_thread() {
    let mut ui = ui();
    list(&mut ui, &["chosen"]);
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, id, &args, true);
    let (id, args) = request(key(&mut ui, KeyCode::Char('y')), true);
    key(&mut ui, KeyCode::Esc);
    assert!(key(&mut ui, KeyCode::Delete).is_empty());
    assert!(ui.view().status.contains("削除を実行中"));
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: id,
        args,
        execute: true,
        result: Err(ApiError::new(ErrorCode::InvalidState, "fixture live Run")),
    });
    request(key(&mut ui, KeyCode::Delete), false);
}

#[test]
fn worker_panic_reply_releases_global_deletion_ownership_for_another_thread() {
    for close_dialog in [false, true] {
        let mut ui = ui();
        list(&mut ui, &["chosen", "next"]);
        let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
        accept(&mut ui, id, &args, true);
        let (id, args) = request(key(&mut ui, KeyCode::Char('y')), true);
        if close_dialog {
            key(&mut ui, KeyCode::Esc);
        }
        ui.apply(UiUpdate::ThreadDeletion {
            request_id: id,
            args,
            execute: true,
            result: Err(ApiError::new(
                ErrorCode::IoFailed,
                "削除処理が予期せず停止しました。結果は未確認です。一覧を再取得してください",
            )
            .details(json!({"deletion_outcome_uncertain": true}))),
        });
        assert!(ui.view().status.contains("削除結果は未確認です"));
        assert!(!ui.view().status.contains("削除できません"));
        let rendered = screen(&mut ui);
        assert!(rendered.contains("削除結果は未確認です"), "{rendered}");
        assert!(!rendered.contains("削除できません"), "{rendered}");
        assert_eq!(
            ui.threads().len(),
            2,
            "uncertain deletion must not remove rows"
        );
        if !close_dialog {
            key(&mut ui, KeyCode::Esc);
        }
        key(&mut ui, KeyCode::Down);
        let (_, next) = request(key(&mut ui, KeyCode::Delete), false);
        assert_eq!(next.thread_id, "next");
    }
}

#[test]
fn late_delete_failure_survives_a_closed_browser_in_the_status_line() {
    let mut ui = ui();
    list(&mut ui, &["chosen"]);
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, id, &args, true);
    let (id, args) = request(key(&mut ui, KeyCode::Char('y')), true);
    key(&mut ui, KeyCode::Esc);
    key(&mut ui, KeyCode::Esc);
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: id,
        args,
        execute: true,
        result: Err(ApiError::new(
            ErrorCode::InvalidState,
            "fixture live Run started",
        )),
    });
    assert!(!ui.threads_visible());
    assert!(ui.view().status.contains("fixture live Run started"));
    list(&mut ui, &["chosen"]);
    assert!(screen(&mut ui).contains("fixture live Run started"));
}

#[test]
fn modified_cancel_keys_do_not_close_confirmation_but_plain_escape_does() {
    let mut ui = ui();
    list(&mut ui, &["chosen"]);
    request(key(&mut ui, KeyCode::Delete), false);
    for code in [KeyCode::Char('n'), KeyCode::Char('N'), KeyCode::Esc] {
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            assert!(
                ui.event(Event::Key(KeyEvent::new(code, modifiers)))
                    .is_empty()
            );
            assert!(screen(&mut ui).contains("会話の削除確認"));
        }
    }
    assert!(key(&mut ui, KeyCode::Esc).is_empty());
    assert!(!screen(&mut ui).contains("会話の削除確認"));
    assert_eq!(ui.threads().len(), 1);
}

#[test]
fn rejected_deletion_admission_releases_inflight_target_for_retry() {
    let mut ui = ui();
    list(&mut ui, &["chosen"]);
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, id, &args, true);
    let actions = key(&mut ui, KeyCode::Char('y'));
    let action = actions
        .iter()
        .find(|a| matches!(a, UiAction::ThreadDeletion { execute: true, .. }))
        .unwrap();
    ui.action_rejected(action);
    assert!(screen(&mut ui).contains("削除は未受付"));
    key(&mut ui, KeyCode::Esc);
    request(key(&mut ui, KeyCode::Delete), false);
}

#[test]
fn deletion_notice_is_preserved_until_display_then_not_replayed_on_reopen() {
    let mut ui = ui();
    list(&mut ui, &["chosen"]);
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, id, &args, true);
    let (id, args) = request(key(&mut ui, KeyCode::Char('y')), true);
    key(&mut ui, KeyCode::Esc);
    key(&mut ui, KeyCode::Esc);
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: id,
        args,
        execute: true,
        result: Err(ApiError::new(
            ErrorCode::InvalidState,
            "once-only browser notice",
        )),
    });
    list(&mut ui, &["chosen"]);
    assert!(screen(&mut ui).contains("once-only browser notice"));
    key(&mut ui, KeyCode::Esc);
    // The main status intentionally retains the outcome, so assert the browser
    // search/header row rather than the full screen's status line.
    list(&mut ui, &["chosen"]);
    let image = screen(&mut ui);
    assert!(
        !image
            .lines()
            .any(|line| line.contains("once-only browser notice") && line.contains("検索"))
    );
}

#[test]
fn a_second_target_cannot_overlap_an_inflight_deletion() {
    let mut ui = ui();
    list(&mut ui, &["parent", "child"]);
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, id, &args, true);
    let (id, args) = request(key(&mut ui, KeyCode::Char('y')), true);
    key(&mut ui, KeyCode::Esc);
    key(&mut ui, KeyCode::Down);
    assert!(key(&mut ui, KeyCode::Delete).is_empty());
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: id,
        args,
        execute: true,
        result: Err(ApiError::new(
            ErrorCode::InvalidState,
            "fixture completed rejection",
        )),
    });
    request(key(&mut ui, KeyCode::Delete), false);
}

#[test]
fn an_overlay_covering_the_browser_does_not_consume_an_unseen_deletion_notice() {
    let mut ui = ui();
    list(&mut ui, &["chosen"]);
    let (id, args) = request(key(&mut ui, KeyCode::Delete), false);
    accept(&mut ui, id, &args, true);
    let (id, args) = request(key(&mut ui, KeyCode::Char('y')), true);
    key(&mut ui, KeyCode::Esc);
    key(&mut ui, KeyCode::F(1));
    ui.apply(UiUpdate::ThreadDeletion {
        request_id: id,
        args,
        execute: true,
        result: Err(ApiError::new(
            ErrorCode::InvalidState,
            "covered deletion notice",
        )),
    });
    let covered = screen(&mut ui);
    assert!(
        !covered
            .lines()
            .any(|line| line.contains("covered deletion notice") && line.contains("検索"))
    );
    key(&mut ui, KeyCode::Esc);
    key(&mut ui, KeyCode::Esc);
    assert!(!ui.threads_visible());
    list(&mut ui, &["chosen"]);
    let shown = screen(&mut ui);
    assert!(
        shown
            .lines()
            .any(|line| line.contains("covered deletion notice") && line.contains("検索"))
    );
}
