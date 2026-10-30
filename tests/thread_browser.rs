use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use shellboy::{config::Config, protocol::ThreadSummary, ui::*};

fn item(id: &str, parent: Option<&str>) -> ThreadSummary {
    ThreadSummary {
        activity: shellboy::activity::ActivityPhase::Idle,
        latest_run_stop_reason: None,
        revisit: None,
        thread_id: id.into(),
        title: format!("Title {id}"),
        cwd: "/synthetic/project".into(),
        parent_id: parent.map(String::from),
        state: "idle".into(),
        updated_at: "2026-10-25T10:00:00Z".into(),
        excerpt: "# 見出し\n\n本文".into(),
    }
}
fn key(ui: &mut UiState, code: KeyCode) -> Vec<UiAction> {
    ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}
fn open() -> UiState {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.insert("/threads", false);
    key(&mut ui, KeyCode::Enter);
    ui
}
fn page(ui: &mut UiState, cursor: Option<&str>, items: Vec<ThreadSummary>, next: Option<&str>) {
    ui.apply(UiUpdate::Threads {
        read_sequence: 0,
        generation: ui.list_generation(),
        requested_cursor: cursor.map(String::from),
        threads: items,
        display_times: vec![],
        next_cursor: next.map(String::from),
    });
}
fn selected(ui: &UiState) -> &str {
    &ui.threads()[ui.thread_selection()].thread_id
}

#[test]
fn tree_keys_follow_visible_order_and_open_the_original_target() {
    let mut ui = open();
    page(
        &mut ui,
        None,
        vec![
            item("child", Some("parent")),
            item("parent", None),
            item("grandchild", Some("child")),
            item("other", None),
        ],
        None,
    );
    assert_eq!(selected(&ui), "parent");
    key(&mut ui, KeyCode::Down);
    assert_eq!(selected(&ui), "child");
    key(&mut ui, KeyCode::Left); // collapse the child's descendants
    key(&mut ui, KeyCode::Down);
    assert_eq!(selected(&ui), "other");
    key(&mut ui, KeyCode::Up);
    assert_eq!(selected(&ui), "child");
    key(&mut ui, KeyCode::Right); // expand
    key(&mut ui, KeyCode::Right);
    assert_eq!(selected(&ui), "grandchild");
    key(&mut ui, KeyCode::Left);
    assert_eq!(selected(&ui), "child");
    key(&mut ui, KeyCode::Left); // collapse
    key(&mut ui, KeyCode::Left);
    assert_eq!(selected(&ui), "parent");
    key(&mut ui, KeyCode::Left);
    key(&mut ui, KeyCode::Down);
    assert_eq!(selected(&ui), "other");
    assert!(
        key(&mut ui, KeyCode::Enter)
            .iter()
            .any(|a| matches!(a,UiAction::SelectThread{thread_id,..}if thread_id=="other"))
    );
    assert!(!ui.threads_visible());
}

#[test]
fn additional_pages_merge_by_id_preserve_selection_and_reject_stale_cursors() {
    let mut ui = open();
    // Model real server ordering: the orphan child is newest, and its
    // parent is old enough to arrive on the subsequent cursor page.
    let mut child = item("child", Some("parent"));
    child.updated_at = "2026-10-25T10:00:01Z".into();
    page(
        &mut ui,
        None,
        vec![child, item("other", None)],
        Some("page2"),
    );
    assert_eq!(selected(&ui), "child");
    let request = key(&mut ui, KeyCode::PageDown);
    assert!(matches!(&request[..],[UiAction::LoadThreads{cursor:Some(c),..}]if c=="page2"));
    assert!(key(&mut ui, KeyCode::PageDown).is_empty());
    page(&mut ui, Some("wrong"), vec![item("wrong", None)], None);
    assert_eq!(ui.threads().len(), 2);
    let mut changed = item("other", None);
    changed.title = "updated".into();
    let mut parent = item("parent", None);
    parent.updated_at = "2026-10-25T09:59:59Z".into();
    page(&mut ui, Some("page2"), vec![changed, parent], Some("page3"));
    assert_eq!(ui.threads().len(), 3);
    assert_eq!(selected(&ui), "child");
    assert_eq!(
        ui.threads()
            .iter()
            .find(|t| t.thread_id == "other")
            .unwrap()
            .title,
        "updated"
    );
    key(&mut ui, KeyCode::Home);
    assert_eq!(selected(&ui), "other"); // saved root ordering is retained
    key(&mut ui, KeyCode::End);
    assert_eq!(selected(&ui), "child");
    let old = ui.list_generation();
    key(&mut ui, KeyCode::Char('q'));
    ui.apply(UiUpdate::Threads {
        read_sequence: 0,
        generation: old,
        requested_cursor: Some("page3".into()),
        threads: vec![item("late", None)],
        display_times: vec![],
        next_cursor: None,
    });
    assert!(ui.threads().iter().all(|t| t.thread_id != "late"));
    page(&mut ui, None, vec![item("query-result", None)], None);
    assert_eq!(ui.threads().len(), 1);
    assert_eq!(selected(&ui), "query-result");
}

#[test]
fn failure_can_retry_and_filter_backspace_preserves_unicode_graphemes() {
    let mut ui = open();
    page(&mut ui, None, vec![item("first", None)], Some("next"));
    key(&mut ui, KeyCode::PageDown);
    ui.apply(UiUpdate::ThreadListFailed {
        generation: ui.list_generation(),
        error: "fixture error".into(),
    });
    assert!(!key(&mut ui, KeyCode::PageDown).is_empty());
    ui.apply(UiUpdate::ThreadListFailed {
        generation: ui.list_generation(),
        error: "fixture error".into(),
    });
    let action = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(
        &action[..],
        [UiAction::LoadThreads { cursor: None, .. }]
    ));
    key(&mut ui, KeyCode::Char('e'));
    key(&mut ui, KeyCode::Char('\u{301}'));
    key(&mut ui, KeyCode::Backspace);
    assert!(ui.thread_filter().is_empty());
    assert_eq!(selected(&ui), "first");
}

#[test]
fn loaded_list_is_bounded_and_new_filter_replaces_it() {
    let mut ui = open();
    page(
        &mut ui,
        None,
        (0..2100).map(|i| item(&format!("{i}"), None)).collect(),
        Some("more"),
    );
    assert_eq!(ui.threads().len(), 2000);
    assert!(key(&mut ui, KeyCode::PageDown).is_empty());
    key(&mut ui, KeyCode::Char('x'));
    page(&mut ui, None, vec![item("x", None)], None);
    assert_eq!(ui.threads().len(), 1);
}

#[test]
fn current_thread_is_initial_selection_even_with_a_different_local_draft() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.insert("keep this local draft", false);
    ui.select_thread("active".into());
    ui.insert("/threads", false);
    key(&mut ui, KeyCode::Enter);
    page(
        &mut ui,
        None,
        vec![item("other", None), item("active", None)],
        None,
    );
    assert_eq!(selected(&ui), "active");
    assert!(
        ui.threads()
            .iter()
            .any(|t| t.thread_id.starts_with("draft:"))
    );
}

#[test]
fn a_failed_new_filter_cannot_reuse_the_old_filter_cursor() {
    let mut ui = open();
    page(&mut ui, None, vec![item("first", None)], Some("old-cursor"));
    key(&mut ui, KeyCode::Char('x'));
    ui.apply(UiUpdate::ThreadListFailed {
        generation: ui.list_generation(),
        error: "failed filter".into(),
    });
    assert!(ui.threads_next_cursor().is_none());
    assert!(key(&mut ui, KeyCode::PageDown).is_empty());
}
