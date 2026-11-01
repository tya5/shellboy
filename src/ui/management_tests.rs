use super::*;

fn fixture_cwd() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\fixture")
    } else {
        PathBuf::from("/fixture")
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn context(action: &UiAction) -> ManagementContext {
    match action {
        UiAction::SubmittedCommand { action, .. } => context(action),
        UiAction::ManagementIntent { context, .. } => context.clone(),
        _ => panic!("missing management display context: {action:?}"),
    }
}
fn preview_attempt(actions: &[UiAction]) -> u64 {
    fn unwrap(action: &UiAction) -> Option<u64> {
        match action {
            UiAction::SubmittedCommand { action, .. }
            | UiAction::ManagementIntent { action, .. } => unwrap(action),
            UiAction::IndexPreview { attempt, .. } => Some(*attempt),
            _ => None,
        }
    }
    actions
        .iter()
        .find_map(unwrap)
        .expect("index preview action")
}
fn request(ui: &mut UiState) -> ManagementContext {
    ui.insert("/status", false);
    let actions = ui.event(key(KeyCode::Enter));
    context(&actions[0])
}
fn report(context: ManagementContext, title: &str) -> UiUpdate {
    UiUpdate::ScopedManagement {
        context,
        update: Box::new(UiUpdate::StructuredManagement {
            work_status: true,
            request_id: 1,
            title: title.into(),
            fields: vec![PresentationField {
                value_omitted: false,
                key: "/run_id".into(),
                label: "Run".into(),
                value: "old-run-complete-id".into(),
                display: None,
            }],
            omitted: false,
        }),
    }
}
#[test]
fn closed_queued_report_and_error_cannot_reopen_or_replace_a_newer_frame_or_other_thread() {
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.select_thread("a".into());
    let old = request(&mut ui);
    ui.event(key(KeyCode::Esc));
    let status = ui.view().status.clone();
    ui.apply(report(old.clone(), "old report"));
    assert!(ui.management.is_none());
    assert_eq!(ui.view().status, status);
    let new = request(&mut ui);
    ui.apply(report(old.clone(), "late old report"));
    assert_ne!(ui.management.as_ref().unwrap().title, "late old report");
    ui.apply(report(new.clone(), "new report"));
    assert_eq!(ui.management.as_ref().unwrap().title, "new report");
    ui.select_thread("b".into());
    let status = ui.view().status.clone();
    ui.apply(UiUpdate::ScopedManagement {
        context: new,
        update: Box::new(UiUpdate::Status {
            thread_id: None,
            text: "late error from a".into(),
        }),
    });
    assert!(ui.management.is_none());
    assert_eq!(ui.view().status, status);
    assert!(
        ui.operation_context(true).run.is_empty(),
        "closed management fields cannot seed a new operation"
    );
    ui.select_thread("a".into());
    ui.apply(report(old, "old report on the same owner again"));
    assert!(
        ui.management.is_none(),
        "a new selection epoch also rejects the same old owner"
    );
}
#[test]
fn leaving_a_detail_page_drops_its_late_page_without_invalidating_the_whole_frame() {
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    let owner = request(&mut ui);
    ui.apply(report(owner.clone(), "current"));
    let state = ui.management.structured_mut().unwrap();
    state.detail = Some(management::ManagementPage {
        observed_at: "synthetic".into(),
        source_request_id: 1,
        stale: None,
        pointer: "/run_id".into(),
        text: "first page".into(),
        next_cursor: Some("next".into()),
    });
    state.pending_pointer = Some("/run_id".into());
    ui.event(key(KeyCode::Esc));
    assert!(ui.management.is_some());
    assert_eq!(ui.management.epoch, owner.epoch);
    ui.apply(UiUpdate::ScopedManagement {
        context: owner,
        update: Box::new(UiUpdate::ManagementDetail {
            detail_request_id: 0,
            request_id: 1,
            pointer: "/run_id".into(),
            text: "late page".into(),
            next_cursor: None,
        }),
    });
    assert!(ui.management.structured().unwrap().detail.is_none());
}
#[test]
fn index_preview_is_invalidated_on_close_and_same_scope_reopening_but_not_detail_requests() {
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.insert("/index all", false);
    let actions = ui.event(key(KeyCode::Enter));
    let old = context(&actions[0]);
    let old_attempt = preview_attempt(&actions);
    let args = ui.management.scope.clone().unwrap();
    ui.management.pending = Some(Command::IndexBackfill(args.clone()));
    ui.event(key(KeyCode::Esc));
    assert!(ui.management.scope.is_none());
    assert!(ui.management.pending.is_none());
    ui.insert("/index all", false);
    let actions = ui.event(key(KeyCode::Enter));
    let current = context(&actions[0]);
    let current_attempt = preview_attempt(&actions);
    ui.apply(UiUpdate::ScopedManagement {
        context: old,
        update: Box::new(UiUpdate::IndexPreviewReady {
            args: args.clone(),
            attempt: old_attempt,
            text: "stale estimate".into(),
        }),
    });
    assert!(ui.confirmation.is_none());
    assert!(matches!(
        ui.management.preview,
        management::IndexPreviewState::Loading(_)
    ));
    // IndexStatus is not the work-status overview. Keep the production index
    // mode while its structured report races the cost preview.
    ui.apply(UiUpdate::ScopedManagement {
        context: current.clone(),
        update: Box::new(UiUpdate::StructuredManagement {
            work_status: false,
            request_id: 1,
            title: "index report".into(),
            fields: vec![PresentationField {
                value_omitted: false,
                key: "/paused".into(),
                label: "一時停止".into(),
                value: "true".into(),
                display: None,
            }],
            omitted: false,
        }),
    });
    let detail = ui.event(key(KeyCode::Enter));
    assert_eq!(
        context(&detail[0]).epoch,
        current.epoch,
        "detail belongs to the same index display session"
    );
    ui.management.pending = Some(Command::IndexBackfill(args.clone()));
    ui.apply(UiUpdate::ScopedManagement {
        context: current,
        update: Box::new(UiUpdate::IndexPreviewReady {
            args,
            attempt: current_attempt,
            text: "current estimate".into(),
        }),
    });
    assert!(
        ui.confirmation.is_some(),
        "valid parallel estimate remains usable"
    );
}

#[test]
fn operation_defaults_follow_fresh_activity_without_retargeting_an_open_form() {
    use super::activity::{ActivityPhase, ActivitySnapshot};
    fn command(action: &UiAction) -> &Command {
        match action {
            UiAction::ManagementIntent { action, .. }
            | UiAction::SubmittedCommand { action, .. } => command(action),
            UiAction::Manage(command) => command,
            _ => panic!("expected typed management command: {action:?}"),
        }
    }
    let old = "00000000-0000-0000-0000-000000000001";
    let new = "00000000-0000-0000-0000-000000000002";
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.select_thread("a".into());
    let generation = ui.view().generation;
    let update = |epoch, run: &str, phase| UiUpdate::Activity {
        thread_id: "a".into(),
        generation,
        epoch,
        observed_at: std::time::Instant::now(),
        snapshot: ActivitySnapshot {
            phase,
            run_id: Some(run.into()),
            ..ActivitySnapshot::default()
        },
    };
    ui.apply(update(1, old, ActivityPhase::Recovery));
    ui.event(key(KeyCode::F(8)));
    ui.event(key(KeyCode::Enter));
    ui.apply(update(2, new, ActivityPhase::Completed));
    let actions = ui.event(key(KeyCode::Enter));
    assert!(matches!(command(&actions[0]), Command::RunRetry(args) if args.run_id == old));
    ui.event(key(KeyCode::Esc));
    ui.event(key(KeyCode::F(8)));
    ui.apply(update(1, old, ActivityPhase::Recovery));
    ui.event(Event::Paste("/stop".into()));
    ui.event(key(KeyCode::Enter));
    let actions = ui.event(key(KeyCode::Enter));
    assert!(matches!(command(&actions[0]), Command::RunStop(args) if args.run_id == new));
    ui.event(key(KeyCode::F(8)));
    ui.select_thread("b".into());
    assert!(
        ui.operations.is_none(),
        "another conversation closes the old operation frame"
    );
}

#[test]
fn frame_content_and_status_navigation_do_not_depend_on_display_title() {
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    let owner = request(&mut ui);
    ui.apply(report(owner, "翻訳済みの別タイトル"));
    let actions = ui.event(key(KeyCode::Char('r')));
    fn follow(action: &UiAction) -> bool {
        match action {
            UiAction::ManagementIntent { action, .. } => follow(action),
            UiAction::FollowManagement {
                mode: ManagementFollow::Refresh,
                ..
            } => true,
            _ => false,
        }
    }
    assert!(actions.iter().any(follow));
    ui.management.open_text(ManagementView {
        title: "別の本文".into(),
        text: "text".into(),
        scroll: 0,
    });
    assert!(ui.management.structured().is_none());
    assert!(
        ui.operation_context(true).run.is_empty(),
        "replaced content cannot seed a new operation"
    );
    ui.execute_operation(Command::ThreadList(
        crate::protocol::ThreadListArgs::default(),
    ));
    ui.apply(UiUpdate::StructuredManagement {
        work_status: false,
        request_id: 10,
        title: "作業状態".into(),
        fields: vec![],
        omitted: false,
    });
    assert!(
        !ui.event(key(KeyCode::Char('r'))).iter().any(follow),
        "a matching title alone never enables status refresh"
    );
}

#[test]
fn mutation_owner_survives_saved_draft_install_receipt_body_eviction_and_other_reports() {
    let mut config = Config::default();
    config.cache.body_bytes = 0;
    let mut ui = UiState::new(&config, fixture_cwd());
    ui.select_thread("a".into());
    let placeholder = ui.view().draft.draft_id.clone();
    let owner = ManagementContext {
        epoch: ui.management.epoch,
        owner_draft: placeholder.clone(),
        owner_thread: Some("a".into()),
    };
    ui.management.owner = Some(owner.clone());
    ui.management.open_text(ManagementView {
        title: "受付待ち".into(),
        text: String::new(),
        scroll: 0,
    });
    let saved = DraftSnapshot::default();
    let saved_id = saved.draft_id.clone();
    let details = serde_json::from_value(serde_json::json!({"agents_path":"/AGENTS.md","thread_id":"a","title":"saved","cwd":fixture_cwd(),"connection":"fixture","model":"fixture","children_count":0,"shells_count":0})).unwrap();
    ui.apply(UiUpdate::Thread {
        generation: ui.view().generation,
        details,
        draft: Some(saved),
    });
    assert_ne!(placeholder, saved_id);
    assert_eq!(ui.view().draft.draft_id, saved_id);
    ui.apply(report(owner.clone(), "same Thread, installed saved draft"));
    assert_eq!(
        ui.management.as_ref().unwrap().title,
        "same Thread, installed saved draft"
    );
    ui.event(key(KeyCode::Esc));
    ui.insert("later message", false);
    let send = ui.event(key(KeyCode::Enter));
    let (draft_id, revision) = send
        .iter()
        .find_map(|action| match action {
            UiAction::Send {
                draft_id, revision, ..
            } => Some((draft_id.clone(), *revision)),
            _ => None,
        })
        .unwrap();
    ui.apply(UiUpdate::Receipt {
        draft_id: draft_id.clone(),
        revision,
        thread_id: Some("a".into()),
        status: ReceiptStatus::Accepted,
        message: None,
    });
    assert_eq!(
        ui.view().draft.draft_id,
        saved_id,
        "normal send acknowledgement keeps the owner ID"
    );
    let text = "old body".repeat(1024);
    ui.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: "a".into(),
        generation: ui.view().generation,
        nodes: vec![UiNode {
            node_id: "old-node".into(),
            role: "assistant".into(),
            text: text.clone(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
            presentation: NodePresentation::default(),
        }],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 1,
    });
    assert_eq!(ui.view().nodes().len(), 1);
    ui.select_thread("b".into());
    let current = request(&mut ui);
    ui.apply(report(current, "newest report on b"));
    let status_b = ui.view().status.clone();
    let views = ui.views.len();
    ui.trim_bodies();
    assert_eq!(ui.views.len(), views);
    assert!(
        ui.views
            .values()
            .find(|view| view.thread_id.as_deref() == Some("a"))
            .unwrap()
            .nodes()
            .is_empty(),
        "only the old body is evicted"
    );
    let notice = "停止要求を受け付けました · Runの停止 · 照会ID fixture:receipt";
    for update in [
        report(owner, "stale mutation report"),
        UiUpdate::Status {
            thread_id: Some("a".into()),
            text: notice.into(),
        },
    ] {
        ui.apply(update);
    }
    assert_eq!(ui.management.as_ref().unwrap().title, "newest report on b");
    assert_eq!(ui.view().status, status_b);
    ui.select_thread("a".into());
    assert!(ui.management.is_none(), "the old report never reopens");
    assert_eq!(ui.view().status, notice);
    assert_eq!(ui.view().draft.draft_id, saved_id);
}

#[test]
fn received_list_opens_an_off_path_original_and_rejects_closed_list_results() {
    fn owner(action: &UiAction) -> ManagementContext {
        match action {
            UiAction::SubmittedCommand { action, .. } => owner(action),
            UiAction::ManagementIntent { context, .. } => context.clone(),
            other => panic!("{other:?}"),
        }
    }
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.select_thread("a".into());
    ui.insert("/received", false);
    let actions = ui.event(key(KeyCode::Enter));
    let context = owner(&actions[0]);
    let report = || UiUpdate::ScopedManagement {
        context: context.clone(),
        update: Box::new(UiUpdate::ReceivedList {
            title: "受信記録 · 35件".into(),
            fields: vec![PresentationField {
                key: "/received/source/old-audit".into(),
                label: "古い失敗".into(),
                value: "old-audit".into(),
                ..Default::default()
            }],
        }),
    };
    ui.apply(report());
    let actions = ui.event(key(KeyCode::Enter));
    assert!(matches!(&actions[0],UiAction::LoadOriginal{node_id,..}if node_id=="old-audit"));
    ui.event(key(KeyCode::Esc));
    ui.event(key(KeyCode::Esc));
    ui.apply(report());
    assert!(ui.management.is_none());
}

#[test]
fn recalled_return_to_slash_draft_uses_the_same_scope_and_respects_overlay_ownership() {
    fn answer(ui: &mut UiState, request: &UiAction, item: Option<&str>) {
        let UiAction::RecallInput {
            thread_id,
            generation,
            request_id,
            ..
        } = request
        else {
            panic!("{request:?}")
        };
        ui.apply(UiUpdate::RecalledInput {
            thread_id: thread_id.clone(),
            generation: *generation,
            request_id: *request_id,
            result: Ok(item.map(|text| RecalledPrompt {
                ordinal: 3,
                node_id: "old-message".into(),
                text: text.into(),
            })),
        });
    }
    for overlay in [false, true] {
        let mut ui = UiState::new(&Config::default(), fixture_cwd());
        ui.select_thread("a".into());
        ui.insert("/status ", false);
        let up = ui.event(key(KeyCode::Up)).remove(0);
        answer(&mut ui, &up, Some("old prompt"));
        let down = ui.event(key(KeyCode::Down)).remove(0);
        assert!(ui.event(key(KeyCode::Enter)).is_empty());
        if overlay {
            ui.event(key(KeyCode::F(1)));
        }
        answer(&mut ui, &down, None);
        let deferred = ui.take_deferred_actions();
        if overlay {
            assert!(deferred.is_empty());
            assert_eq!(ui.view().draft.text, "/status ");
        } else {
            let owner = context(&deferred[0]);
            ui.event(key(KeyCode::Esc));
            ui.apply(report(owner, "closed deferred report"));
            assert!(
                ui.management.is_none(),
                "the recalled command's late report cannot reopen a popup"
            );
        }
    }
}

#[test]
fn index_status_key_refreshes_the_index_after_a_structured_retry_receipt() {
    for input in ["/index", "/index all", "/index space fixture-space"] {
        let mut ui = UiState::new(&Config::default(), fixture_cwd());
        ui.insert(input, false);
        let actions = ui.event(key(KeyCode::Enter));
        let owner = context(&actions[0]);
        let expected = ui.management.scope.clone().unwrap_or_default();
        ui.apply(UiUpdate::ScopedManagement {
            context: owner,
            update: Box::new(UiUpdate::StructuredManagement {
                work_status: false,
                request_id: 42,
                title: "index retry accepted".into(),
                fields: vec![PresentationField {
                    value_omitted: false,
                    key: "/accepted".into(),
                    label: "受付".into(),
                    value: "true".into(),
                    display: None,
                }],
                omitted: false,
            }),
        });
        let actions = ui.event(key(KeyCode::Char('s')));
        assert_eq!(actions.len(), 1);
        let UiAction::ManagementIntent { action, .. } = &actions[0] else {
            panic!("index refresh must retain its management epoch");
        };
        assert!(
            matches!(action.as_ref(), UiAction::Manage(Command::IndexStatus(args)) if args == &expected),
            "{input}: {actions:?}"
        );
    }
}

#[test]
fn duplicate_index_preview_does_not_replace_completed_evidence() {
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.insert("/index all", false);
    let actions = ui.event(key(KeyCode::Enter));
    let owner = context(&actions[0]);
    let attempt = preview_attempt(&actions);
    let args = ui.management.scope.clone().unwrap();
    let reply = |text: &str| UiUpdate::ScopedManagement {
        context: owner.clone(),
        update: Box::new(UiUpdate::IndexPreviewReady {
            args: args.clone(),
            attempt,
            text: text.into(),
        }),
    };
    ui.apply(reply("first estimate"));
    ui.apply(reply("duplicate estimate"));
    ui.apply(UiUpdate::ScopedManagement {
        context: owner.clone(),
        update: Box::new(UiUpdate::IndexPreviewFailed {
            args: args.clone(),
            attempt,
            error: "late failure".into(),
        }),
    });
    assert!(matches!(&ui.management.preview,
        management::IndexPreviewState::Ready(_, text) if text == "first estimate"));
}

#[test]
fn older_same_scope_preview_and_rejection_cannot_replace_new_attempt() {
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.insert("/index all", false);
    let first = ui.event(key(KeyCode::Enter));
    let old_owner = context(&first[0]);
    let old_attempt = preview_attempt(&first);
    let old_action = first
        .iter()
        .find(|action| {
            fn is_preview(action: &UiAction) -> bool {
                match action {
                    UiAction::SubmittedCommand { action, .. }
                    | UiAction::ManagementIntent { action, .. } => is_preview(action),
                    UiAction::IndexPreview { .. } => true,
                    _ => false,
                }
            }
            is_preview(action)
        })
        .unwrap()
        .clone();
    let args = ui.management.scope.clone().unwrap();
    let second = ui.event(key(KeyCode::Char('p')));
    let new_owner = context(&second[0]);
    let new_attempt = preview_attempt(&second);
    assert_ne!(old_owner.epoch, new_owner.epoch);
    assert_ne!(old_attempt, new_attempt);
    let ready = |owner: &ManagementContext, attempt, text: &str| UiUpdate::ScopedManagement {
        context: owner.clone(),
        update: Box::new(UiUpdate::IndexPreviewReady {
            args: args.clone(),
            attempt,
            text: text.into(),
        }),
    };
    ui.apply(ready(&old_owner, old_attempt, "old owner"));
    ui.apply(ready(&new_owner, old_attempt, "old attempt"));
    ui.apply(UiUpdate::ScopedManagement {
        context: new_owner.clone(),
        update: Box::new(UiUpdate::IndexPreviewFailed {
            args: args.clone(),
            attempt: old_attempt,
            error: "old error".into(),
        }),
    });
    ui.action_rejected(&old_action);
    assert!(matches!(
        ui.management.preview,
        management::IndexPreviewState::Loading(_)
    ));
    assert!(ui.management.is_some());
    ui.apply(ready(&new_owner, new_attempt, "new"));
    assert!(
        matches!(&ui.management.preview, management::IndexPreviewState::Ready(_, text) if text == "new")
    );
}

#[test]
fn rejected_current_index_preview_finishes_loading_without_closing_owner() {
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.insert("/index all", false);
    ui.event(key(KeyCode::Enter));
    let actions = ui.event(key(KeyCode::Char('p')));
    let attempt = preview_attempt(&actions);
    ui.action_rejected(&actions[0]);
    assert!(ui.management.is_some());
    assert!(
        matches!(&ui.management.preview, management::IndexPreviewState::Failed(_, text) if text.contains("未受付"))
    );
    let owner = context(&actions[0]);
    let args = ui.management.scope.clone().unwrap();
    ui.apply(UiUpdate::ScopedManagement {
        context: owner,
        update: Box::new(UiUpdate::IndexPreviewReady {
            args,
            attempt,
            text: "late".into(),
        }),
    });
    assert!(matches!(
        &ui.management.preview,
        management::IndexPreviewState::Failed(_, _)
    ));
}

#[test]
fn changed_confirmation_retains_preview_and_rejects_old_reply_and_execution() {
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.insert("/index all", false);
    let first = ui.event(key(KeyCode::Enter));
    let owner = context(&first[0]);
    let attempt = preview_attempt(&first);
    let args = ui.management.scope.clone().unwrap();
    ui.apply(UiUpdate::ScopedManagement {
        context: owner.clone(),
        update: Box::new(UiUpdate::IndexPreviewReady {
            args: args.clone(),
            attempt,
            text: "captured result at fixed time".into(),
        }),
    });
    assert!(ui.event(key(KeyCode::Char('b'))).is_empty());
    assert!(matches!(ui.confirmation, Some(Confirmation::Index { .. })));
    ui.apply(UiUpdate::ConfirmationSourcesChanged);
    assert!(
        matches!(&ui.confirmation, Some(Confirmation::StaleIndex { detail }) if detail.contains("captured result at fixed time"))
    );
    assert!(ui.event(key(KeyCode::Char('y'))).is_empty());
    assert!(matches!(
        ui.confirmation,
        Some(Confirmation::StaleIndex { .. })
    ));
    ui.event(key(KeyCode::Esc));
    let renewed = ui.event(key(KeyCode::Char('p')));
    let new_owner = context(&renewed[0]);
    let new_attempt = preview_attempt(&renewed);
    assert_ne!(attempt, new_attempt);
    ui.apply(UiUpdate::ScopedManagement {
        context: new_owner.clone(),
        update: Box::new(UiUpdate::IndexPreviewReady {
            args: args.clone(),
            attempt,
            text: "late".into(),
        }),
    });
    assert!(matches!(
        ui.management.preview,
        management::IndexPreviewState::Loading(_)
    ));
    ui.apply(UiUpdate::ScopedManagement {
        context: new_owner,
        update: Box::new(UiUpdate::IndexPreviewReady {
            args,
            attempt: new_attempt,
            text: "renewed".into(),
        }),
    });
    assert!(
        matches!(&ui.management.preview, management::IndexPreviewState::Ready(_, text) if text == "renewed")
    );
}

#[test]
fn tiny_index_preview_renders_visible_omission_marker() {
    use ratatui::{Terminal, backend::TestBackend};
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.event(Event::Resize(20, 10));
    ui.insert("/index all", false);
    let actions = ui.event(key(KeyCode::Enter));
    let owner = context(&actions[0]);
    let attempt = preview_attempt(&actions);
    let args = ui.management.scope.clone().unwrap();
    ui.apply(UiUpdate::ScopedManagement {
        context: owner,
        update: Box::new(UiUpdate::IndexPreviewReady {
            args,
            attempt,
            text: "many facts\nsecond fact\n2026-10-30T04:05:06Z".into(),
        }),
    });
    let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut ui))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut rendered = String::new();
    for y in 0..10 {
        let mut x = 0;
        while x < 20 {
            let symbol = buffer[(x, y)].symbol();
            rendered.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        rendered.push('\n');
    }
    assert!(rendered.contains("省略"), "{rendered}");
}

#[test]
fn external_completion_is_acknowledged_only_by_visible_matching_owner() {
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.select_thread("a".into());
    let scope = request(&mut ui);
    let response = |scope: ManagementContext, generation| UiUpdate::ScopedManagement {
        context: scope,
        update: Box::new(UiUpdate::ExternalManagement {
            generation,
            owner: "thread:a".into(),
            update: Box::new(UiUpdate::StructuredManagement {
                work_status: false,
                request_id: generation,
                title: "external".into(),
                fields: vec![],
                omitted: false,
            }),
        }),
    };
    ui.deferred_actions.clear();
    ui.help_visible = true;
    ui.help_above_popups = true;
    ui.apply(response(scope.clone(), 1));
    assert!(
        !ui.deferred_actions
            .iter()
            .any(|a| matches!(a, UiAction::ExternalObserved { .. }))
    );
    ui.help_visible = false;
    ui.apply(response(scope.clone(), 2));
    assert!(
        ui.deferred_actions
            .iter()
            .any(|a| matches!(a, UiAction::ExternalObserved { generation: 2 }))
    );
    ui.deferred_actions.clear();
    ui.event(key(KeyCode::Esc));
    ui.apply(response(scope, 3));
    assert!(
        !ui.deferred_actions
            .iter()
            .any(|a| matches!(a, UiAction::ExternalObserved { .. }))
    );
}

#[test]
fn failed_index_preview_retains_only_stale_same_scope_evidence() {
    let mut ui = UiState::new(&Config::default(), fixture_cwd());
    ui.insert("/index all", false);
    let actions = ui.event(key(KeyCode::Enter));
    let args = ui.management.scope.clone().unwrap();
    assert!(ui.management.accept_preview(
        preview_attempt(&actions),
        args.clone(),
        "previous preview".into(),
    ));

    let failed = ui.management.begin_preview(args.clone());
    ui.management
        .fail_preview(failed, args.clone(), "failed".into());
    assert!(matches!(
        &ui.management.preview,
        management::IndexPreviewState::Failed(_, _)
    ));
    assert!(ui.event(key(KeyCode::Char('b'))).is_empty());

    let pending = ui.management.begin_preview(args.clone());
    ui.management.invalidate_index_preview();
    assert!(
        matches!(&ui.management.preview, management::IndexPreviewState::Stale(_, text) if text == "previous preview")
    );
    assert!(
        !ui.management
            .accept_preview(pending, args.clone(), "late".into())
    );

    ui.management.reset_preview();
    ui.management.begin_preview(args);
    ui.management.invalidate_index_preview();
    assert!(
        matches!(&ui.management.preview, management::IndexPreviewState::Stale(_, text) if text != "previous preview")
    );
}
