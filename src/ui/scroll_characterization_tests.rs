//! Characterization of conversation-history scroll state (#118, stage 1).
//!
//! These tests pin what the scroll flags (`follow`, `loading`, `scroll.request`,
//! `scroll.pending`, `history_rebase_pending`, `unseen`, `anchor`, `within`,
//! `moved_during_read`, `initialized`) do today, before any refactor. They drive
//! the real `UiState` input/update path, never set the flags directly except to
//! build a starting position, and compare the flag tuple plus visible rows.
//! Expected values record current behaviour, not a reviewed ideal. Cases that
//! look doubtful carry `// characterization: current behaviour, see #118`.
use super::*;
use crossterm::event::{Event, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

const THREAD: &str = "scroll-test";

fn node(id: &str, text: String) -> UiNode {
    UiNode {
        node_id: id.into(),
        role: "assistant".into(),
        total_chars: text.chars().count(),
        text,
        ..Default::default()
    }
}
fn numbered(prefix: &str, rows: usize) -> String {
    (0..rows).map(|i| format!("{prefix}{i:03}\n")).collect()
}
fn fixture(
    nodes: Vec<UiNode>,
    total: u64,
    first_ordinal: u64,
    width: u16,
    height: u16,
) -> (UiState, Terminal<TestBackend>) {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread(THREAD.into());
    ui.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: THREAD.into(),
        generation: ui.view().generation,
        total_nodes: total,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal,
        selection_revision: 0,
    });
    (ui, Terminal::new(TestBackend::new(width, height)).unwrap())
}
fn single(rows: usize) -> (UiState, Terminal<TestBackend>) {
    fixture(vec![node("long", numbered("R", rows))], 1, 0, 60, 33)
}
/// Ten 100-row nodes. `evicted` clears that node's cached text (length kept).
fn ten(evicted: Option<usize>) -> (UiState, Terminal<TestBackend>, Vec<UiNode>) {
    let complete: Vec<UiNode> = (0..10)
        .map(|i| node(&format!("n-{i}"), numbered(&format!("N{i}-"), 100)))
        .collect();
    let mut nodes = complete.clone();
    if let Some(i) = evicted {
        nodes[i].text.clear();
    }
    let (ui, terminal) = fixture(nodes, 10, 0, 60, 33);
    (ui, terminal, complete)
}
fn rows(ui: &mut UiState, terminal: &mut Terminal<TestBackend>) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    terminal.draw(|frame| draw(frame, ui)).unwrap();
    let buffer = terminal.backend().buffer();
    (2..buffer.area.height - 11)
        .map(|y| {
            let mut row = String::new();
            let mut x = 1;
            while x < buffer.area.width - 1 {
                let symbol = buffer[(x, y)].symbol();
                row.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            row.trim_end().to_string()
        })
        .collect()
}
fn wheel(ui: &mut UiState, kind: MouseEventKind) -> Vec<UiAction> {
    ui.event(Event::Mouse(MouseEvent {
        kind,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }))
}
fn up(ui: &mut UiState) -> Vec<UiAction> {
    wheel(ui, MouseEventKind::ScrollUp)
}
fn down(ui: &mut UiState) -> Vec<UiAction> {
    wheel(ui, MouseEventKind::ScrollDown)
}
fn key(ui: &mut UiState, code: KeyCode) -> Vec<UiAction> {
    ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}
fn ctrl_end(ui: &mut UiState) -> Vec<UiAction> {
    ui.event(Event::Key(KeyEvent::new(
        KeyCode::End,
        KeyModifiers::CONTROL,
    )))
}
fn set_anchor(ui: &mut UiState, node_id: &str, char_offset: usize) {
    let (generation, view_revision) = (ui.view().generation, ui.view().navigation.view_revision);
    ui.apply(UiUpdate::ViewState {
        thread_id: THREAD.into(),
        generation,
        view_revision,
        anchor: ViewAnchor {
            node_id: Some(node_id.into()),
            char_offset,
        },
        follow: false,
    });
}
/// The full scroll flag tuple, as one comparable line.
fn snap(ui: &UiState) -> String {
    let v = ui.view();
    format!(
        "follow={} loading={} request={:?} pending={} rebase={} unseen={} anchor={}@{} within={} moved={} initialized={} revision={}",
        v.navigation.follow(),
        v.navigation.is_loading(),
        v.navigation
            .request()
            .map(|(r, n)| (*r - v.navigation.view_revision.min(*r), n.clone())),
        v.navigation.pending(),
        v.navigation.is_rebasing(),
        v.navigation.unseen(),
        v.navigation.anchor.node_id.as_deref().unwrap_or("-"),
        v.navigation.anchor.char_offset,
        v.navigation.scroll.within,
        v.navigation.moved_during_read(),
        v.navigation.scroll.initialized(),
        v.navigation.view_revision,
    )
}
fn node_window(ui: &mut UiState, node: UiNode) {
    let (generation, view_revision) = (ui.view().generation, ui.view().navigation.view_revision);
    ui.apply(UiUpdate::NodeWindow {
        thread_id: THREAD.into(),
        generation,
        view_revision,
        node,
    });
}
fn first_text(rows: &[String]) -> String {
    rows.iter()
        .find(|r| !r.trim().is_empty())
        .map(|r| r.trim().to_string())
        .unwrap_or_default()
}
fn last_text(rows: &[String]) -> String {
    rows.iter()
        .rev()
        .find(|r| !r.trim().is_empty())
        .map(|r| r.trim().to_string())
        .unwrap_or_default()
}

// ---- follow / tail ----

#[test]
fn following_tail_is_the_initial_state_and_render_does_not_change_it() {
    let (mut ui, mut t) = single(200);
    let before = snap(&ui);
    let first = rows(&mut ui, &mut t);
    let after_first = snap(&ui);
    let second = rows(&mut ui, &mut t);
    assert_eq!(first, second, "repeated render is stable");
    assert_eq!(after_first, snap(&ui), "second render writes no flag");
    assert!(last_text(&first) == "R199", "{first:?}");
    assert!(
        before.starts_with(
            "follow=true loading=false request=None pending=0 rebase=false unseen=false"
        )
    );
    // The first render is the one that initializes the visual scroll cache.
    assert!(before.contains("initialized=false"), "{before}");
    assert!(after_first.contains("initialized=true"), "{after_first}");
    assert!(ui.view().navigation.scroll.row() > 0 && ui.view().navigation.scroll.height == 33 - 13);
}

#[test]
fn following_tail_tracks_streamed_growth_without_unseen() {
    let (mut ui, mut t) = single(200);
    rows(&mut ui, &mut t);
    let grown = numbered("R", 210);
    let generation = ui.view().generation;
    ui.apply(UiUpdate::Stream {
        thread_id: THREAD.into(),
        generation,
        node_id: "long".into(),
        role: "assistant".into(),
        total_chars: grown.chars().count(),
        text: grown,
        base_char_offset: 0,
        reasoning_ranges: vec![],
        presentation: Default::default(),
        ack: None,
    });
    let screen = rows(&mut ui, &mut t);
    assert!(last_text(&screen) == "R209", "{screen:?}");
    assert!(ui.view().navigation.follow() && !ui.view().navigation.unseen());
}

#[test]
fn scroll_down_while_following_is_a_no_op_that_stays_following() {
    let (mut ui, mut t) = single(200);
    let tail = rows(&mut ui, &mut t);
    let before = snap(&ui);
    assert!(before.contains("anchor=-@0"), "{before}");
    assert!(down(&mut ui).is_empty());
    assert_eq!(rows(&mut ui, &mut t), tail);
    // characterization: current behaviour, see #118
    // The rows and `follow` do not change, but `scroll()` still records the
    // top source row as the model anchor and bumps `view_revision`, so a
    // no-op wheel at the tail writes state (which would invalidate any read
    // keyed to the previous revision).
    let after = snap(&ui);
    assert!(
        after.starts_with("follow=true loading=false request=None pending=0"),
        "{after}"
    );
    assert!(after.contains("anchor=long@905"), "{after}");
    assert!(after.ends_with("revision=1"), "{after}");
    assert!(before.ends_with("revision=0"), "{before}");
}

// ---- scroll up / down on cached rows ----

#[test]
fn wheel_up_from_tail_pauses_on_a_source_anchor_with_a_within_row_offset() {
    let (mut ui, mut t) = single(200);
    let tail = rows(&mut ui, &mut t);
    assert!(up(&mut ui).is_empty(), "cached rows need no read");
    let paused = rows(&mut ui, &mut t);
    assert_eq!(
        &paused[3..],
        &tail[..tail.len() - 3],
        "three rows per wheel"
    );
    let v = ui.view();
    assert!(
        !v.navigation.follow() && !v.navigation.is_loading() && v.navigation.request().is_none()
    );
    assert_eq!(v.navigation.pending(), 0);
    assert_eq!(v.navigation.anchor.node_id.as_deref(), Some("long"));
    assert!(v.navigation.anchor.char_offset > 0);
    assert_eq!(
        v.navigation.scroll.anchor, v.navigation.anchor,
        "visual anchor mirrors the model"
    );
}

#[test]
fn paused_wheel_down_short_of_the_tail_stays_paused_then_reaching_it_follows() {
    let (mut ui, mut t) = single(200);
    let tail = rows(&mut ui, &mut t);
    for _ in 0..4 {
        up(&mut ui);
    }
    rows(&mut ui, &mut t);
    for _ in 0..3 {
        down(&mut ui);
        rows(&mut ui, &mut t);
        assert!(!ui.view().navigation.follow(), "short of the tail");
    }
    down(&mut ui);
    assert_eq!(rows(&mut ui, &mut t), tail);
    assert!(ui.view().navigation.follow() && !ui.view().navigation.unseen());
}

#[test]
fn reversal_near_the_tail_returns_to_follow_exactly_at_the_tail() {
    // The #117 near-tail symptom: up one step, then back down.
    let (mut ui, mut t) = single(200);
    let tail = rows(&mut ui, &mut t);
    for steps in 1..=3 {
        for _ in 0..steps {
            up(&mut ui);
        }
        rows(&mut ui, &mut t);
        assert!(!ui.view().navigation.follow());
        for _ in 0..steps {
            down(&mut ui);
            rows(&mut ui, &mut t);
        }
        assert!(ui.view().navigation.follow(), "steps={steps}");
        assert_eq!(rows(&mut ui, &mut t), tail, "steps={steps}");
    }
}

#[test]
fn growth_while_paused_near_the_tail_keeps_the_rows_and_marks_unseen() {
    let (mut ui, mut t) = single(200);
    rows(&mut ui, &mut t);
    up(&mut ui);
    let paused = rows(&mut ui, &mut t);
    let grown = numbered("R", 230);
    let generation = ui.view().generation;
    ui.apply(UiUpdate::Stream {
        thread_id: THREAD.into(),
        generation,
        node_id: "long".into(),
        role: "assistant".into(),
        total_chars: grown.chars().count(),
        text: grown,
        base_char_offset: 0,
        reasoning_ranges: vec![],
        presentation: Default::default(),
        ack: None,
    });
    assert_eq!(rows(&mut ui, &mut t), paused);
    assert!(ui.view().navigation.unseen() && !ui.view().navigation.follow());
    // One wheel down does not reach the new tail, so it stays paused.
    down(&mut ui);
    rows(&mut ui, &mut t);
    assert!(!ui.view().navigation.follow() && ui.view().navigation.unseen());
    // Up remains available (the stopped-growth near-tail case of #117).
    assert!(up(&mut ui).is_empty());
    assert!(!ui.view().navigation.follow());
    // Ctrl+End jumps to the new tail and clears unseen.
    ctrl_end(&mut ui);
    let tail = rows(&mut ui, &mut t);
    assert!(last_text(&tail) == "R229");
    assert!(ui.view().navigation.follow() && !ui.view().navigation.unseen());
}

// ---- reads: gaps, pages, reversal ----

#[test]
fn page_up_into_an_uncached_neighbour_owns_one_read_and_keeps_the_screen() {
    let (mut ui, mut t, complete) = ten(Some(4));
    set_anchor(&mut ui, "n-5", 0);
    let screen = rows(&mut ui, &mut t);
    let actions = key(&mut ui, KeyCode::PageUp);
    let reads: Vec<_> = actions
        .iter()
        .filter(|a| matches!(a, UiAction::LoadNodeWindow { node_id, .. } if node_id == "n-4"))
        .collect();
    assert_eq!(reads.len(), 1, "{actions:?}");
    assert_eq!(actions.len(), 1);
    let v = ui.view();
    assert!(
        v.navigation.is_loading() && v.navigation.request().is_some() && !v.navigation.follow()
    );
    assert!(
        v.navigation.pending() < 0,
        "movement is retained: {}",
        v.navigation.pending()
    );
    assert_eq!(rows(&mut ui, &mut t), screen, "old screen until the reply");
    node_window(&mut ui, complete[4].clone());
    let v = ui.view();
    assert!(!v.navigation.is_loading() && v.navigation.request().is_none());
    assert_eq!(
        v.navigation.pending(),
        0,
        "deferred movement consumed on reply"
    );
    assert!(!v.navigation.follow());
    assert_ne!(rows(&mut ui, &mut t), screen);
}

#[test]
fn same_direction_input_while_awaiting_accumulates_without_another_read() {
    let (mut ui, mut t, complete) = ten(Some(4));
    set_anchor(&mut ui, "n-5", 0);
    rows(&mut ui, &mut t);
    key(&mut ui, KeyCode::PageUp);
    let pending = ui.view().navigation.pending();
    let request = ui.view().navigation.request().cloned();
    // The uncached node is the next one up, so cached rows cannot satisfy this.
    let again = key(&mut ui, KeyCode::PageUp);
    assert!(again.is_empty(), "no duplicate read: {again:?}");
    assert_eq!(
        ui.view().navigation.request().cloned(),
        request,
        "same owner and revision"
    );
    assert!(
        ui.view().navigation.pending() <= pending,
        "movement accumulates"
    );
    node_window(&mut ui, complete[4].clone());
    assert_eq!(ui.view().navigation.pending(), 0);
    // A repeated reply for the same revision finds no owner and changes nothing.
    let settled = snap(&ui);
    let screen = rows(&mut ui, &mut t);
    node_window(&mut ui, complete[4].clone());
    assert_eq!(snap(&ui), settled);
    assert_eq!(rows(&mut ui, &mut t), screen);
}

#[test]
fn reversing_while_awaiting_drops_the_pending_movement() {
    // characterization: current behaviour, see #118
    // Reverse input cancels the deferred movement instead of adding to it, and
    // that reverse input itself is consumed only when cached rows can move.
    let (mut ui, mut t, complete) = ten(Some(4));
    set_anchor(&mut ui, "n-5", 0);
    rows(&mut ui, &mut t);
    key(&mut ui, KeyCode::PageUp);
    assert!(ui.view().navigation.pending() < 0);
    let actions = key(&mut ui, KeyCode::PageDown);
    assert!(actions.is_empty());
    assert_eq!(ui.view().navigation.pending(), 0, "{}", snap(&ui));
    assert!(
        ui.view().navigation.request().is_some(),
        "the read stays owned"
    );
    node_window(&mut ui, complete[4].clone());
    assert!(ui.view().navigation.request().is_none());
    assert_eq!(ui.view().navigation.pending(), 0);
}

#[test]
fn ctrl_end_during_a_read_cancels_it_and_the_old_reply_is_ignored() {
    let (mut ui, mut t, complete) = ten(Some(4));
    set_anchor(&mut ui, "n-5", 0);
    rows(&mut ui, &mut t);
    key(&mut ui, KeyCode::PageUp);
    let stale_revision = ui.view().navigation.view_revision;
    ctrl_end(&mut ui);
    let tail = rows(&mut ui, &mut t);
    let v = ui.view();
    assert!(
        v.navigation.follow() && !v.navigation.is_loading() && v.navigation.request().is_none()
    );
    assert_eq!(v.navigation.pending(), 0);
    assert_ne!(
        v.navigation.view_revision, stale_revision,
        "cancel bumps the fence"
    );
    let settled = snap(&ui);
    let generation = ui.view().generation;
    ui.apply(UiUpdate::NodeWindow {
        thread_id: THREAD.into(),
        generation,
        view_revision: stale_revision,
        node: complete[4].clone(),
    });
    assert_eq!(snap(&ui), settled);
    assert_eq!(rows(&mut ui, &mut t), tail);
}

#[test]
fn load_failure_keeps_pending_for_an_explicit_retry_and_rejection_releases_the_read() {
    for rejected in [false, true] {
        let (mut ui, mut t, _) = ten(Some(4));
        set_anchor(&mut ui, "n-5", 0);
        rows(&mut ui, &mut t);
        let actions = key(&mut ui, KeyCode::PageUp);
        let pending = ui.view().navigation.pending();
        if rejected {
            ui.action_rejected(&actions[0]);
        } else {
            let (generation, view_revision) =
                (ui.view().generation, ui.view().navigation.view_revision);
            ui.apply(UiUpdate::LoadFailed {
                thread_id: THREAD.into(),
                generation,
                view_revision: Some(view_revision),
                message: "read failed".into(),
            });
        }
        let v = ui.view();
        assert!(
            !v.navigation.is_loading() && v.navigation.request().is_none(),
            "rejected={rejected}"
        );
        // characterization: current behaviour, see #118
        // The deferred movement survives the failure; the next scroll replays it
        // (added to the new input) and issues a fresh read. No automatic retry.
        assert_eq!(v.navigation.pending(), pending, "rejected={rejected}");
        let retry = key(&mut ui, KeyCode::PageUp);
        assert!(
            retry
                .iter()
                .any(|a| matches!(a, UiAction::LoadNodeWindow { node_id, .. } if node_id == "n-4")),
            "{retry:?}"
        );
    }
}

#[test]
fn unrelated_refresh_failure_does_not_cancel_the_owned_read() {
    let (mut ui, mut t, _) = ten(Some(4));
    set_anchor(&mut ui, "n-5", 0);
    rows(&mut ui, &mut t);
    key(&mut ui, KeyCode::PageUp);
    let owned = snap(&ui);
    let generation = ui.view().generation;
    ui.apply(UiUpdate::LoadFailed {
        thread_id: THREAD.into(),
        generation,
        view_revision: None,
        message: "background refresh failed".into(),
    });
    assert_eq!(snap(&ui), owned);
}

// ---- page loads preserving anchors ----

#[test]
fn history_page_before_the_oldest_cached_node_is_requested_and_keeps_the_anchor() {
    // Nodes 5..10 of 10 are cached; scrolling up past the first one needs an
    // ordinal-window read, not a node-body read.
    let nodes: Vec<UiNode> = (5..10)
        .map(|i| node(&format!("n-{i}"), numbered(&format!("N{i}-"), 100)))
        .collect();
    let (mut ui, mut t) = fixture(nodes, 10, 5, 60, 33);
    set_anchor(&mut ui, "n-5", 0);
    let screen = rows(&mut ui, &mut t);
    let actions = up(&mut ui);
    let load = actions
        .iter()
        .find_map(|a| match a {
            UiAction::LoadHistoryWindow {
                ordinal, from_end, ..
            } => Some((*ordinal, *from_end)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{actions:?}"));
    assert_eq!(load, (4, true));
    let v = ui.view();
    assert!(v.navigation.is_loading() && !v.navigation.follow());
    assert!(matches!(v.navigation.request(), Some((_, None))));
    assert!(v.navigation.pending() < 0);
    assert_eq!(v.navigation.anchor.node_id.as_deref(), Some("n-5"));
    assert_eq!(
        rows(&mut ui, &mut t),
        screen,
        "last frame stays during the read"
    );
    // The older page arrives, overlapping the cached first node.
    let older: Vec<UiNode> = (3..6)
        .map(|i| node(&format!("n-{i}"), numbered(&format!("N{i}-"), 100)))
        .collect();
    let (generation, view_revision) = (ui.view().generation, ui.view().navigation.view_revision);
    let anchor = ui.view().navigation.anchor.clone();
    ui.apply(UiUpdate::HistoryWindow {
        thread_id: THREAD.into(),
        generation,
        view_revision,
        nodes: older,
        pending: Default::default(),
        read_revision: 0,
        first_ordinal: 3,
        selection_revision: 0,
        total_nodes: 10,
        anchor,
    });
    let v = ui.view();
    assert!(
        !v.navigation.is_loading() && v.navigation.request().is_none() && !v.navigation.follow()
    );
    assert_eq!(v.navigation.pending(), 0);
    assert!(v.first_ordinal <= 3);
    // The retained movement was applied once: the viewport now shows rows that
    // precede the former top, in the older node.
    let after = rows(&mut ui, &mut t);
    assert_eq!(first_text(&after), "N4-098", "{after:?}");
}

// ---- selection-driven scroll ----

#[test]
fn view_state_update_replaces_anchor_and_follow_and_clears_unseen() {
    let (mut ui, mut t) = single(200);
    rows(&mut ui, &mut t);
    ui.view_mut().navigation.set_unseen(true);
    set_anchor(&mut ui, "long", 40);
    let v = ui.view();
    assert!(!v.navigation.follow() && !v.navigation.unseen());
    assert_eq!(v.navigation.anchor.char_offset, 40);
    let screen = rows(&mut ui, &mut t);
    assert!(first_text(&screen).starts_with("R008"), "{screen:?}");
    assert!(ui.view().navigation.scroll.initialized());
}

#[test]
fn tab_into_history_focus_pauses_on_the_visible_top_row_without_a_read() {
    let (mut ui, mut t, _) = ten(None);
    let screen = rows(&mut ui, &mut t);
    assert!(ui.view().navigation.follow());
    let actions = key(&mut ui, KeyCode::Tab);
    assert!(actions.is_empty(), "{actions:?}");
    // characterization: current behaviour, see #118
    // Entering item focus leaves following and pins the screen: `select_history_item`
    // writes follow/anchor/within/row/revision/origin/initialized directly,
    // bypassing `scroll()`, and rebases (not cancels) navigation.
    assert!(!ui.view().navigation.follow(), "{}", snap(&ui));
    assert_eq!(rows(&mut ui, &mut t), screen, "same rows, now paused");
    let v = ui.view();
    assert!(
        !v.navigation.is_loading()
            && v.navigation.request().is_none()
            && v.navigation.pending() == 0
    );
    assert!(v.navigation.scroll.initialized() && !v.navigation.unseen());
}

// ---- resize ----

#[test]
fn resize_keeps_the_source_anchor_and_a_following_tail() {
    let (mut ui, mut t) = single(200);
    let tail = rows(&mut ui, &mut t);
    // Following: a narrower and a taller terminal keep the tail.
    t.backend_mut().resize(40, 33);
    let narrow = rows(&mut ui, &mut t);
    assert!(last_text(&narrow) == "R199", "{narrow:?}");
    assert!(ui.view().navigation.follow());
    t.backend_mut().resize(60, 33);
    assert_eq!(rows(&mut ui, &mut t), tail);
    // Paused: the source anchor is unchanged by width and height changes.
    for _ in 0..5 {
        up(&mut ui);
    }
    let paused = rows(&mut ui, &mut t);
    let anchor = ui.view().navigation.anchor.clone();
    t.backend_mut().resize(40, 39);
    rows(&mut ui, &mut t);
    assert_eq!(ui.view().navigation.anchor, anchor);
    assert!(!ui.view().navigation.follow() && ui.view().navigation.request().is_none());
    assert!(ui.view().navigation.scroll.initialized());
    t.backend_mut().resize(60, 33);
    assert_eq!(
        rows(&mut ui, &mut t),
        paused,
        "round trip restores the rows"
    );
}

#[test]
fn render_writes_viewport_cache_fields_and_then_settles() {
    // characterization: current behaviour, see #118
    // `viewport_row` is a query that also writes row/origin/width/height/
    // revision/anchor/initialized. Pin the written values, then pin that a
    // second identical render writes the same ones.
    let (mut ui, mut t) = single(200);
    rows(&mut ui, &mut t);
    up(&mut ui);
    rows(&mut ui, &mut t);
    let state = |ui: &UiState| {
        let s = &ui.view().navigation.scroll;
        (
            s.row(),
            s.within,
            s.origin,
            s.width,
            s.height,
            s.revision,
            s.anchor.clone(),
            s.initialized(),
        )
    };
    let first = state(&ui);
    assert_eq!(first.3, 58, "text width excludes the border and scrollbar");
    assert_eq!(first.4, 20);
    assert_eq!(first.5, ui.view().history_revision());
    assert_eq!(first.6, ui.view().navigation.anchor);
    assert!(first.7);
    rows(&mut ui, &mut t);
    assert_eq!(state(&ui), first);
}

#[test]
fn paused_anchor_beyond_a_true_end_is_clamped_to_a_full_last_page_on_render() {
    // characterization: current behaviour, see #118
    // A paused anchor near the end of a fully cached thread leaves a blank
    // tail; render pulls the row back to `lines - height` (the "true end" clamp
    // duplicated in viewport_row and scroll), but leaves `follow` false.
    let (mut ui, mut t) = single(200);
    let tail = rows(&mut ui, &mut t);
    set_anchor(&mut ui, "long", numbered("R", 199).chars().count());
    let screen = rows(&mut ui, &mut t);
    assert_eq!(screen, tail);
    assert!(
        !ui.view().navigation.follow(),
        "clamp does not restore follow"
    );
}
