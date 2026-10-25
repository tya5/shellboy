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
    let args = ui.management.scope.clone().unwrap();
    ui.management.pending = Some(Command::IndexBackfill(args.clone()));
    ui.event(key(KeyCode::Esc));
    assert!(ui.management.scope.is_none());
    assert!(ui.management.pending.is_none());
    ui.insert("/index all", false);
    let actions = ui.event(key(KeyCode::Enter));
    let current = context(&actions[0]);
    ui.apply(UiUpdate::ScopedManagement {
        context: old,
        update: Box::new(UiUpdate::IndexPreviewReady {
            args: args.clone(),
            text: "stale estimate".into(),
        }),
    });
    assert!(ui.confirmation.is_none());
    assert!(ui.management.preview.is_none());
    ui.apply(report(current.clone(), "index report"));
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
    let details = serde_json::from_value(serde_json::json!({"thread_id":"a","title":"saved","cwd":fixture_cwd(),"connection":"fixture","model":"fixture","children_count":0,"shells_count":0})).unwrap();
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
