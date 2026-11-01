//! Owner forms are a strict, usable subset of the executable catalogue.
use super::*;

fn context() -> Context {
    Context {
        thread: "thread".into(),
        run: "run".into(),
        current_run: "run".into(),
        turn: "turn".into(),
        node: "node".into(),
        cwd: PathBuf::from("/tmp"),
        follows_current: true,
        ..Default::default()
    }
}
fn set(form: &mut Form, name: &str, value: &str) {
    form.fields
        .iter_mut()
        .find(|field| field.name == name)
        .unwrap()
        .value = value.into();
}
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn screen(operations: &Operations) -> String {
    screen_at(operations, 120, 45)
}
fn screen_at(operations: &Operations, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| operations.draw(frame, frame.area()))
        .unwrap();
    // Continuation cells of a wide glyph are not independent characters.
    let mut text = String::new();
    for y in 0..height {
        let mut x = 0;
        while x < width {
            let symbol = terminal.backend().buffer()[(x, y)].symbol();
            text.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        text.push('\n');
    }
    text
}
#[test]
fn owner_forms_cover_every_owner_catalogue_identity_without_client_stubs() {
    let owner = OperationId::ALL
        .iter()
        .filter(|id| id.spec().route == crate::operations::Route::Owner)
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let implemented = OpKind::OWNER
        .iter()
        .map(|kind| kind.catalogue())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(implemented.len(), 45);
    assert_eq!(implemented, owner);
    for kind in OpKind::OWNER {
        let form = Form::new(*kind, &context());
        assert!(!form.fields.is_empty());
        for index in 0..form.fields.len() {
            let mut selected = Form::new(*kind, &context());
            selected.selected = index;
            assert!(
                selected.selected_metadata().is_some(),
                "{kind:?}: {}",
                selected.fields[index].name
            );
        }
    }
}
#[test]
fn six_new_owner_forms_build_real_commands_and_preserve_optional_arguments() {
    let c = context();
    let mut cancel = Form::new(OpKind::MessageCancel, &c);
    set(&mut cancel, "messages", "first second");
    assert!(
        matches!(cancel.command(&c.cwd).unwrap(),Command::MessageCancel(args)if args.message_ids==["first","second"])
    );
    set(&mut cancel, "messages", "");
    assert!(cancel.command(&c.cwd).is_err());
    let mut revisit = Form::new(OpKind::RevisitSet, &c);
    set(&mut revisit, "text", "next instruction");
    assert!(
        matches!(revisit.command(&c.cwd).unwrap(),Command::RevisitSet(args)if args.run_id.as_deref()==Some("run")&&args.text=="next instruction")
    );
    assert!(
        matches!(Form::new(OpKind::RevisitGet,&c).command(&c.cwd).unwrap(),Command::RevisitGet(args)if args.run_id.as_deref()==Some("run"))
    );
    assert!(
        matches!(Form::new(OpKind::RevisitCancel,&c).command(&c.cwd).unwrap(),Command::RevisitCancel(args)if args.run_id.as_deref()==Some("run"))
    );
    assert!(
        matches!(Form::new(OpKind::IndexPreview,&c).command(&c.cwd).unwrap(),Command::IndexPreview(args)if args.thread_ids==["thread"])
    );
    let mut probe = Form::new(OpKind::IndexProbe, &c);
    set(&mut probe, "dimensions", "256");
    assert!(
        matches!(probe.command(&c.cwd).unwrap(),Command::IndexProbe(args)if args.dimensions==Some(256))
    );
    set(&mut probe, "dimensions", "0");
    assert!(probe.command(&c.cwd).is_err());
}
#[test]
fn previously_missing_flags_have_typed_effects() {
    let c = context();
    let mut send = Form::new(OpKind::MessageSend, &c);
    set(&mut send, "text", "hello");
    set(&mut send, "sender_label", "operator");
    set(&mut send, "now", "true");
    assert!(
        matches!(send.command(&c.cwd).unwrap(),Command::MessageSend(args)if args.now&&args.sender_label.as_deref()==Some("operator"))
    );
    let mut history = Form::new(OpKind::HistoryGet, &c);
    set(&mut history, "final_answer_only", "true");
    assert!(
        matches!(history.command(&c.cwd).unwrap(),Command::HistoryGet(args)if args.final_answer_only)
    );
    let mut rebuild = Form::new(OpKind::IndexRebuild, &c);
    set(&mut rebuild, "dimensions", "512");
    assert!(
        matches!(rebuild.command(&c.cwd).unwrap(),Command::IndexRebuild(args)if args.dimensions==Some(512))
    );
    let mut usage = Form::new(OpKind::UsageGet, &c);
    set(&mut usage, "run", "other-run");
    assert!(usage.command(&c.cwd).is_err());
    set(&mut usage, "thread", "");
    assert!(
        matches!(usage.command(&c.cwd).unwrap(),Command::UsageGet(args)if args.thread_id.is_none()&&args.run_id.as_deref()==Some("other-run"))
    );
    set(&mut usage, "turn", "other-turn");
    assert!(usage.command(&c.cwd).is_err());
}
#[test]
fn all_compact_modes_use_existing_range_validation_and_effect_verbs() {
    let c = context();
    for (mode, from, to) in [
        ("deferred", "", ""),
        ("maximal", "", ""),
        ("spill", "a", "b"),
        ("summary", "a", "b"),
        ("trim", "", "b"),
        ("omit", "a", "b"),
        ("restore", "a", ""),
    ] {
        let mut form = Form::new(OpKind::Compact, &c);
        set(&mut form, "action", mode);
        set(&mut form, "from_node", from);
        set(&mut form, "to_node", to);
        let command = form.command(&c.cwd).unwrap();
        match command {
            Command::Compact(_) => assert_eq!(mode, "deferred"),
            Command::ContextEdit(args) => {
                assert_eq!(
                    args.action,
                    serde_json::from_value::<ContextEditAction>(serde_json::json!(mode)).unwrap()
                )
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(form.verb(), "実行");
    }
    for (mode, from, to) in [
        ("deferred", "a", ""),
        ("maximal", "a", "b"),
        ("spill", "a", ""),
        ("summary", "", ""),
        ("trim", "a", "b"),
        ("restore", "", "b"),
    ] {
        let mut form = Form::new(OpKind::Compact, &c);
        set(&mut form, "action", mode);
        set(&mut form, "from_node", from);
        set(&mut form, "to_node", to);
        assert!(form.command(&c.cwd).is_err(), "{mode}");
    }
    assert_eq!(Form::new(OpKind::RunGet, &c).verb(), "表示");
    assert_eq!(Form::new(OpKind::MessageSend, &c).verb(), "送信");
}
#[test]
fn action_cache_survives_paints_navigation_and_same_group_activity_changes() {
    let mut operations = Operations::new(context());
    let builds = operations.row_builds.get();
    for _ in 0..10 {
        let _ = screen(&operations);
        operations.key(key(KeyCode::Down));
        operations.key(key(KeyCode::Up));
    }
    assert_eq!(operations.row_builds.get(), builds);
    operations.refresh_activity(
        ActivityPhase::Idle,
        "different-run".into(),
        "different-turn".into(),
    );
    assert_eq!(operations.row_builds.get(), builds);
    operations.paste("index probe");
    assert_eq!(operations.rows(), [OpKind::IndexProbe]);
    assert_eq!(operations.row_builds.get(), builds + 1);
    let filtered = screen(&operations);
    assert!(!filtered.contains("その他の操作"));
    assert!(!filtered.contains("よく使う"));
    assert!(filtered.contains("1/1件"));
    operations.key(key(KeyCode::Enter));
    assert_eq!(
        operations.form.as_ref().unwrap().operation,
        OpKind::IndexProbe
    );
}
#[test]
fn headers_never_become_actions_and_help_preserves_edits_and_references() {
    let mut operations = Operations::new(context());
    let actions = operations.rows().to_vec();
    let rendered = screen(&operations);
    assert!(rendered.contains("その他の操作"));
    assert!(rendered.contains("61件"));
    operations.key(key(KeyCode::Home));
    for (index, expected) in actions.iter().enumerate() {
        assert_eq!(OpKind::ALL[operations.selected], *expected);
        if index == 0 || operations.section(actions[index - 1]) != operations.section(*expected) {
            let heading = match operations.section(*expected) {
                0 => "この会話で",
                1 => "よく使う",
                _ => "その他の操作",
            };
            assert!(
                screen(&operations).contains(heading),
                "group heading for {expected:?}"
            );
        }
        operations.key(key(KeyCode::Down));
    }
    operations.select(OpKind::RunGet);
    operations.paste("-edited");
    let before = operations
        .form
        .as_ref()
        .unwrap()
        .value("run")
        .unwrap()
        .to_owned();
    operations.key(key(KeyCode::F(1)));
    let help = screen(&operations);
    assert!(help.contains("CLIの入力項目"));
    assert!(help.contains("選択項目"));
    operations.key(key(KeyCode::End));
    let _ = screen_at(&operations, 70, 14);
    assert!(operations.form.as_ref().unwrap().help_scroll.get() > 0);
    operations.refresh_activity(ActivityPhase::Recovery, "new-run".into(), "new-turn".into());
    operations.key(key(KeyCode::Esc));
    assert_eq!(
        operations.form.as_ref().unwrap().value("run").unwrap(),
        before
    );
    assert!(!operations.form.as_ref().unwrap().help);
}

#[test]
fn list_discloses_saved_changes_and_runtime_control_before_opening_forms() {
    for (filter, expected, absent) in [
        ("thread set", "[変更]", "[制御]"),
        ("run stop", "[制御]", "[変更]"),
        ("run get", "", "[制御]"),
    ] {
        let mut operations = Operations::new(context());
        operations.paste(filter);
        assert_eq!(operations.rows().len(), 1, "{filter}");
        let rendered = screen(&operations);
        let row = rendered
            .lines()
            .find(|line| line.contains(operations.rows()[0].label()))
            .unwrap();
        assert!(row.contains(expected), "{filter}: {row}");
        assert!(!row.contains(absent), "{filter}: {row}");
        if filter == "run get" {
            assert!(!row.contains("[変更]"));
        }
        assert!(operations.form.is_none(), "markers precede any form");
    }
}

#[test]
fn compact_summary_limits_match_new_main_and_reject_other_modes() {
    let c = context();
    let mut form = Form::new(OpKind::Compact, &c);
    set(&mut form, "action", "summary");
    set(&mut form, "from_node", "a");
    set(&mut form, "to_node", "b");
    set(&mut form, "max_chars", "1200");
    set(&mut form, "max_percent", "12.5");
    let Command::ContextEdit(args) = form.command(&c.cwd).unwrap() else {
        panic!("summary command required")
    };
    assert_eq!(args.max_chars, Some(1200));
    assert_eq!(args.max_percent, Some(12.5));
    for mode in ["deferred", "maximal", "spill", "trim", "omit", "restore"] {
        set(&mut form, "action", mode);
        assert!(form.command(&c.cwd).is_err(), "{mode}");
    }
    set(&mut form, "action", "summary");
    for value in ["0", "101", "NaN", "inf", "-1"] {
        set(&mut form, "max_percent", value);
        assert!(form.command(&c.cwd).is_err(), "{value}");
    }
}

#[test]
fn operation_effect_legend_survives_eighty_column_popup_clipping() {
    let rendered = screen_at(&Operations::new(context()), 80, 24);
    assert!(rendered.contains("[変更]保存"));
    assert!(rendered.contains("[制御]処理"));
}

#[test]
fn readback_periods_memory_and_resumable_settlement_use_current_typed_args() {
    let c = context();
    let mut shells = Form::new(OpKind::ShellList, &c);
    set(&mut shells, "exit_code", "-1");
    set(&mut shells, "command_contains", "printf");
    assert!(
        matches!(shells.command(&c.cwd).unwrap(), Command::ShellList(args)
        if args.exit_code == Some(-1) && args.command_contains.as_deref() == Some("printf"))
    );
    for kind in [OpKind::UsageGet, OpKind::CompactList] {
        let mut form = Form::new(kind, &c);
        set(&mut form, "since", "2026-10-30T00:00:00Z");
        set(&mut form, "until", "2026-10-31T00:00:00Z");
        let value = serde_json::to_value(form.command(&c.cwd).unwrap()).unwrap();
        assert_eq!(value["args"]["since"], "2026-10-30T00:00:00Z");
        assert_eq!(value["args"]["until"], "2026-10-31T00:00:00Z");
    }
    let mut memory = Form::new(OpKind::Status, &c);
    set(&mut memory, "memory", "true");
    assert!(
        memory.command(&c.cwd).is_err(),
        "memory conflicts with selected Thread filter"
    );
    set(&mut memory, "thread", "");
    assert!(matches!(memory.command(&c.cwd).unwrap(), Command::Status(args) if args.memory));
    let mut wait = Form::new(OpKind::Wait, &c);
    wait.selected = wait
        .fields
        .iter()
        .position(|field| field.name == "settled_states")
        .unwrap();
    let metadata = wait.selected_metadata().unwrap();
    assert_eq!(metadata.default_origin, DefaultOrigin::Literal);
    assert_eq!(
        metadata.tui_default,
        Some(crate::operations::DEFAULT_SETTLEMENT_STATES)
    );
    set(&mut wait, "condition", "thread_settled");
    set(&mut wait, "target", "thread-a thread-b");
    set(&mut wait, "settled_states", "completed recovery_wait");
    set(&mut wait, "after", "previous-wait-cursor");
    let Command::Wait(args) = wait.command(&c.cwd).unwrap() else {
        panic!()
    };
    assert_eq!(args.after.as_deref(), Some("previous-wait-cursor"));
    assert!(
        matches!(&args.conditions[0], WaitCondition::ThreadSettled {thread_ids, states}
        if thread_ids == &["thread-a", "thread-b"] && states == &["completed", "recovery_wait"])
    );
}

#[test]
fn grouped_usage_form_preserves_catalogue_fields_optional_defaults_and_typed_values() {
    let context=Context::default();
    let mut form=Form::new(OpKind::UsageGet,&context);
    let Command::UsageGet(default)=form.command(std::path::Path::new(".")).unwrap() else {panic!("usage")};
    assert!(default.group_by.is_none() && default.timezone.is_none() && default.limit.is_none() && default.cursor.is_none());
    for (field,value) in [("group_by","thread"),("timezone","UTC"),("limit","12"),("cursor","opaque")] {
        form.fields.iter_mut().find(|f|f.name==field).unwrap().value=value.into();
    }
    let Command::UsageGet(args)=form.command(std::path::Path::new(".")).unwrap() else {panic!("usage")};
    assert_eq!(args.group_by.as_deref(),Some("thread"));assert_eq!(args.timezone.as_deref(),Some("UTC"));
    assert_eq!(args.limit,Some(12));assert_eq!(args.cursor.as_deref(),Some("opaque"));assert!(args.thread_id.is_none());
}
