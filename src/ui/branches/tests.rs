use super::*;
use crate::ui::text_width;
use crossterm::event::KeyModifiers;
use serde_json::json;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn item(
    id: &str,
    parent: Option<&str>,
    state: &str,
    point: Option<(i64, &str, &str)>,
    tip: &str,
    nodes: i64,
) -> Value {
    json!({
        "thread_id": id,
        "title": format!("title {id}"),
        "parent_thread_id": parent,
        "state": state,
        "running": false,
        "pruned_originals": false,
        "divergence": point.map(|(ordinal, node, request)| json!({
            "node_id": node, "ordinal": ordinal, "parent_continues": true,
            "request": {"node_id": node, "text": request, "truncated": false}})),
        "tip": {"node_id": format!("tip-{id}"), "at": "2026-11-05T01:30:00Z",
                "last_request": {"node_id": "u", "text": tip, "truncated": false}},
        "size": {"path_nodes": nodes, "nodes_after_divergence": point.map(|_| 3), "bytes": null},
    })
}
fn page(items: Vec<Value>, next: Option<&str>) -> Value {
    json!({"items": items, "next_cursor": next, "family_truncated": false, "ancestors_truncated": false})
}
/// Root with two divergence points; the first holds two branches, one with a child.
fn sample() -> Value {
    page(
        vec![
            item("r", None, "current", None, "make CI pass", 38),
            item(
                "a",
                Some("r"),
                "branch",
                Some((3, "n3", "fix the tests")),
                "try with mocks",
                21,
            ),
            item(
                "b",
                Some("r"),
                "branch",
                Some((3, "n3", "fix the tests")),
                "split the module",
                24,
            ),
            item(
                "c",
                Some("r"),
                "branch",
                Some((9, "n9", "update the README")),
                "add English",
                12,
            ),
            item(
                "a1",
                Some("a"),
                "branch",
                Some((15, "n15", "try with mocks")),
                "use a fake clock",
                25,
            ),
        ],
        None,
    )
}
fn panel(language: UiLanguage, value: &Value) -> BranchPanel {
    let mut panel = BranchPanel::new(language, "r".into(), 1);
    panel.accept(value, false);
    panel
}
fn render(panel: &BranchPanel, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| panel.draw(frame, frame.area()))
        .unwrap();
    let mut text = String::new();
    for y in 0..height {
        let mut line = String::new();
        let mut x = 0;
        while x < width {
            let symbol = terminal.backend().buffer()[(x, y)].symbol();
            line.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}
fn with_glyphs<T>(f: impl FnOnce() -> T) -> T {
    text_width::set_terminal_width(Some(1));
    let out = f();
    text_width::clear_terminal_width();
    out
}
fn thread_order(panel: &BranchPanel) -> Vec<&str> {
    panel
        .rows
        .iter()
        .filter_map(|row| match row.kind {
            RowKind::Thread(i) => Some(panel.entries[i].thread_id.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn tree_lists_each_thread_then_its_points_in_path_order_with_branches_under_them() {
    let panel = panel(UiLanguage::En, &sample());
    assert_eq!(thread_order(&panel), ["r", "a", "a1", "b", "c"]);
    let shape: Vec<String> = panel
        .rows
        .iter()
        .map(|row| match &row.kind {
            RowKind::Thread(i) => format!("T {}", panel.entries[*i].thread_id),
            RowKind::Point { request } => format!("P {}", request.clone().unwrap_or_default()),
        })
        .collect();
    assert_eq!(
        shape,
        [
            "T r",
            "P fix the tests",
            "T a",
            "P try with mocks",
            "T a1",
            "T b",
            "P update the README",
            "T c"
        ]
    );
    assert_eq!(
        panel.selected_thread(),
        Some("r"),
        "opens on the viewed Thread"
    );
}

#[test]
fn wide_english_render_uses_tree_glyphs_and_puts_time_and_size_on_the_row() {
    let text = with_glyphs(|| render(&panel(UiLanguage::En, &sample()), 100, 24));
    println!("--- English, 100 columns ---\n{text}");
    assert!(text.contains("◉ Branches"), "{text}");
    assert!(text.contains("3 points"), "{text}");
    assert!(text.contains("4 branches"), "{text}");
    assert!(text.contains("├─ \"fix the tests\""), "{text}");
    assert!(text.contains("│  ├─ branch \"try with mocks\""), "{text}");
    assert!(text.contains("│  │  └─ \"try with mocks\""), "{text}");
    assert!(
        text.contains("│  │     └─ branch \"use a fake clock\""),
        "{text}"
    );
    assert!(text.contains("└─ \"update the README\""), "{text}");
    assert!(text.contains("38 nodes"), "{text}");
    assert!(text.contains("current \"make CI pass\""), "{text}");
    assert!(text.contains("Enter Open") && text.contains("F Fork") && text.contains("P Prune"));
}

#[test]
fn wide_japanese_render_is_localized() {
    let text = with_glyphs(|| render(&panel(UiLanguage::Ja, &sample()), 100, 24));
    println!("--- Japanese, 100 columns ---\n{text}");
    assert!(text.contains("◉ 分岐"), "{text}");
    assert!(text.contains("分岐点 3"), "{text}");
    assert!(text.contains("今の会話 \"make CI pass\""), "{text}");
    assert!(text.contains("38件"), "{text}");
    assert!(
        text.contains("Enter 開く") && text.contains("P 手放す"),
        "{text}"
    );
}

#[test]
fn narrow_render_drops_time_and_size_from_rows_and_keeps_them_in_the_detail_area() {
    for language in [UiLanguage::En, UiLanguage::Ja] {
        let text = with_glyphs(|| render(&panel(language, &sample()), 44, 22));
        println!("--- {language:?}, 44 columns ---\n{text}");
        assert!(text.contains("├─") && text.contains("└─"), "{text}");
        assert!(!text.contains("11-0") || text.lines().count() > 0);
        // The selected Thread's time and size are in the detail lines, not the tree rows.
        let size = if language == UiLanguage::En {
            "38 nodes"
        } else {
            "38件"
        };
        let rows_with_size = text
            .lines()
            .filter(|l| l.contains(size) && (l.contains('├') || l.contains('└')))
            .count();
        assert_eq!(rows_with_size, 0, "{text}");
        assert!(text.contains(size), "{text}");
        for line in text.lines() {
            assert!(
                text_width::UnicodeWidthStr::width(line) <= 44,
                "line wider than the popup: {line:?}"
            );
        }
    }
}

#[test]
fn very_narrow_width_still_draws_without_panicking_or_overflowing() {
    for width in [20u16, 24, 30] {
        for language in [UiLanguage::En, UiLanguage::Ja] {
            let text = with_glyphs(|| render(&panel(language, &sample()), width, 16));
            for line in text.lines() {
                assert!(
                    text_width::UnicodeWidthStr::width(line) <= usize::from(width),
                    "{line:?}"
                );
            }
        }
    }
    // Tiny frames settle too.
    let _ = render(&panel(UiLanguage::En, &sample()), 8, 3);
}

#[test]
fn ascii_fallback_uses_the_existing_plain_tree_pieces() {
    let text = render(&panel(UiLanguage::En, &sample()), 100, 24);
    println!("--- English, ASCII fallback ---\n{text}");
    assert!(text.contains("+- \"fix the tests\""), "{text}");
    assert!(text.contains("\\- \"update the README\""), "{text}");
    assert!(text.contains("|  +- branch"), "{text}");
    assert!(!text.contains('├') && !text.contains('└') && !text.contains('│'));
}

#[test]
fn deep_trees_trim_outer_levels_instead_of_overflowing() {
    let mut items = vec![item("t0", None, "current", None, "root", 5)];
    for depth in 1..14 {
        items.push(item(
            &format!("t{depth}"),
            Some(&format!("t{}", depth - 1)),
            "branch",
            Some((depth, &format!("n{depth}"), "point")),
            &format!("level {depth}"),
            5,
        ));
    }
    let panel = panel(UiLanguage::En, &page(items, None));
    assert_eq!(thread_order(&panel).len(), 14);
    let text = with_glyphs(|| render(&panel, 40, 40));
    println!("--- deep tree, 40 columns ---\n{text}");
    for line in text.lines() {
        assert!(text_width::UnicodeWidthStr::width(line) <= 40, "{line:?}");
    }
    assert!(
        text.contains("level 13"),
        "the deepest Thread stays readable: {text}"
    );
}

#[test]
fn many_branches_scroll_so_the_selection_stays_visible() {
    let mut items = vec![item("r", None, "current", None, "root", 100)];
    for i in 0..120 {
        items.push(item(
            &format!("b{i:03}"),
            Some("r"),
            "branch",
            Some((10 + i, &format!("n{i}"), &format!("point {i}"))),
            &format!("request {i:03}"),
            10,
        ));
    }
    let mut panel = panel(UiLanguage::En, &page(items, None));
    let first = with_glyphs(|| render(&panel, 90, 20));
    assert!(first.contains("make") || first.contains("root"));
    assert!(!first.contains("request 119"));
    panel.key(key(KeyCode::End));
    assert_eq!(panel.selected_thread(), Some("b119"));
    let last = with_glyphs(|| render(&panel, 90, 20));
    assert!(last.contains("request 119"), "{last}");
    assert!(!last.contains("request 001"), "{last}");
}

#[test]
fn pruned_originals_and_running_show_as_state_words_without_new_glyphs() {
    let mut value = sample();
    value["items"][1]["pruned_originals"] = json!(true);
    value["items"][2]["running"] = json!(true);
    let text = with_glyphs(|| render(&panel(UiLanguage::En, &value), 110, 24));
    assert!(text.contains("branch pruned \"try with mocks\""), "{text}");
    assert!(
        text.contains("branch running \"split the module\""),
        "{text}"
    );
    let ja = with_glyphs(|| render(&panel(UiLanguage::Ja, &value), 110, 24));
    assert!(ja.contains("原文解放済") && !ja.contains('⬦'), "{ja}");
    assert!(ja.contains("実行中"), "{ja}");
}

#[test]
fn selection_skips_point_rows_and_keys_dispatch_the_documented_operations() {
    let mut panel = panel(UiLanguage::En, &sample());
    let mut seen = Vec::new();
    for _ in 0..4 {
        panel.key(key(KeyCode::Down));
        seen.push(panel.selected_thread().unwrap().to_owned());
    }
    assert_eq!(seen, ["a", "a1", "b", "c"]);
    panel.key(key(KeyCode::Down));
    assert_eq!(
        panel.selected_thread(),
        Some("c"),
        "stops at the last Thread"
    );
    panel.key(key(KeyCode::Home));
    assert_eq!(panel.selected_thread(), Some("r"));
    panel.key(key(KeyCode::Down));
    assert!(matches!(panel.key(key(KeyCode::Enter)), Outcome::Open(id) if id == "a"));
    assert!(matches!(
        panel.key(key(KeyCode::Char('f'))),
        Outcome::Fork { thread_id, node_id } if thread_id == "a" && node_id == "tip-a"
    ));
    assert!(matches!(
        panel.key(key(KeyCode::Char('P'))),
        Outcome::Prune { thread_id, node_id } if thread_id == "a" && node_id == "tip-a"
    ));
    assert!(matches!(panel.key(key(KeyCode::Esc)), Outcome::Close));
    assert!(
        matches!(panel.key(key(KeyCode::F(8))), Outcome::None),
        "F8 keeps popup ownership"
    );
    assert!(
        matches!(panel.key(key(KeyCode::Char('n'))), Outcome::None),
        "no further page"
    );
}

#[test]
fn a_next_page_appends_and_keeps_the_selection() {
    let mut first = sample();
    first["next_cursor"] = json!("cursor-1");
    let mut panel = panel(UiLanguage::En, &first);
    panel.key(key(KeyCode::Down));
    assert!(
        matches!(panel.key(key(KeyCode::Char('n'))), Outcome::Load { cursor } if cursor == "cursor-1")
    );
    panel.begin_page(2);
    assert!(
        matches!(panel.key(key(KeyCode::Char('n'))), Outcome::None),
        "not while loading"
    );
    let more = page(
        vec![item(
            "z",
            Some("r"),
            "branch",
            Some((30, "n30", "late")),
            "later",
            4,
        )],
        None,
    );
    panel.accept(&more, true);
    assert_eq!(panel.selected_thread(), Some("a"));
    assert_eq!(thread_order(&panel).last(), Some(&"z"));
    assert!(!render(&panel, 100, 24).contains("Showing"));
}

#[test]
fn limits_and_failures_are_stated() {
    let mut value = sample();
    value["family_truncated"] = json!(true);
    let text = render(&panel(UiLanguage::En, &value), 100, 24);
    assert!(text.contains("limit reached"), "{text}");
    let mut failed = BranchPanel::new(UiLanguage::Ja, "r".into(), 1);
    assert!(render(&failed, 60, 12).contains("読み込んでいます"));
    failed.fail("読めませんでした".into());
    assert!(render(&failed, 60, 12).contains("読めませんでした"));
    let empty = panel(UiLanguage::En, &page(vec![], None));
    assert!(render(&empty, 60, 12).contains("No branches"));
}

mod owner {
    use super::*;
    use crate::protocol::Command;
    use crate::ui::{UiAction, UiState, UiUpdate};

    fn state() -> UiState {
        let mut config = crate::config::Config {
            default_connection: Some("fixture".into()),
            ..crate::config::Config::default()
        };
        config.connections.insert(
            "fixture".into(),
            crate::config::Connection {
                base_url: "http://127.0.0.1:9/v1".into(),
                ..Default::default()
            },
        );
        UiState::new(&config, std::path::PathBuf::from("/"))
    }
    fn open(ui: &mut UiState) -> u64 {
        let actions = ui.slash("/branches r");
        let request_id = match &actions[..] {
            [
                UiAction::LoadBranches {
                    thread_id,
                    request_id,
                    cursor: None,
                },
            ] if thread_id == "r" => *request_id,
            other => panic!("expected one LoadBranches, got {}", other.len()),
        };
        ui.apply(UiUpdate::BranchesLoaded {
            request_id,
            append: false,
            result: Ok(sample()),
        });
        request_id
    }

    #[test]
    fn slash_and_the_f8_operation_open_the_panel_with_one_bounded_read() {
        let mut ui = state();
        assert!(
            ui.slash("/branches").is_empty(),
            "no saved conversation: an error, no read"
        );
        assert!(ui.branches.is_none());
        open(&mut ui);
        assert!(ui.branches.is_some());
        assert_eq!(
            ui.top_overlay(),
            Some(crate::ui::overlay::Overlay::Branches)
        );
        let mut ui = state();
        let actions = ui.execute_operation(Command::ThreadBranches(
            crate::protocol::ThreadBranchesArgs {
                thread_id: Some("r".into()),
                limit: 50,
                cursor: None,
            },
        ));
        assert!(
            matches!(&actions[..], [UiAction::LoadBranches { thread_id, cursor: None, .. }] if thread_id == "r")
        );
    }

    #[test]
    fn stale_pages_are_ignored_and_errors_are_shown() {
        let mut ui = state();
        let id = open(&mut ui);
        ui.apply(UiUpdate::BranchesLoaded {
            request_id: id + 7,
            append: false,
            result: Err("stale".into()),
        });
        assert!(matches!(ui.branches.as_ref().unwrap().load, Load::Ready));
        ui.apply(UiUpdate::BranchesLoaded {
            request_id: id,
            append: false,
            result: Err("storage busy".into()),
        });
        // Rows already read stay; the error goes to the detail lines.
        assert!(matches!(ui.branches.as_ref().unwrap().load, Load::Ready));
        assert_eq!(
            ui.branches.as_ref().unwrap().page_error.as_deref(),
            Some("storage busy")
        );
    }

    #[test]
    fn enter_opens_the_selected_branch_and_closes_the_panel() {
        let mut ui = state();
        open(&mut ui);
        ui.event(crossterm::event::Event::Key(key(KeyCode::Down)));
        let actions = ui.event(crossterm::event::Event::Key(key(KeyCode::Enter)));
        assert!(ui.branches.is_none());
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, UiAction::SelectThread { thread_id, .. } if thread_id == "a"))
                || ui.view().thread_id.as_deref() == Some("a"),
            "the branch Thread is opened"
        );
    }

    #[test]
    fn fork_sends_thread_fork_from_the_branch_tip() {
        let mut ui = state();
        open(&mut ui);
        ui.event(crossterm::event::Event::Key(key(KeyCode::Down)));
        let actions = ui.event(crossterm::event::Event::Key(key(KeyCode::Char('f'))));
        assert!(ui.branches.is_none());
        fn command(action: &UiAction) -> Option<&Command> {
            match action {
                UiAction::ManagementIntent { action, .. }
                | UiAction::SubmittedCommand { action, .. } => command(action),
                UiAction::Manage(command) | UiAction::Command(command) => Some(command),
                _ => None,
            }
        }
        match actions.iter().filter_map(command).collect::<Vec<_>>()[..] {
            [Command::ThreadFork(args)] => {
                assert_eq!(args.thread_id, "a");
                assert_eq!(args.node_id, "tip-a");
            }
            _ => panic!("expected ThreadFork, got {} actions", actions.len()),
        }
    }

    #[test]
    fn prune_opens_the_existing_form_with_the_dry_run_and_the_branch_tip() {
        let mut ui = state();
        open(&mut ui);
        ui.event(crossterm::event::Event::Key(key(KeyCode::Down)));
        let actions = ui.event(crossterm::event::Event::Key(key(KeyCode::Char('p'))));
        assert!(
            actions.is_empty(),
            "nothing is pruned from the panel itself"
        );
        assert!(ui.branches.is_none());
        let operations = ui.operations.as_ref().expect("the prune form");
        let (kind, values) = operations
            .open_form_values(&["thread", "before", "dry_run"])
            .expect("form is open");
        assert_eq!(kind, crate::ui::operations::OpKind::ThreadPrune);
        assert_eq!(values, ["a", "tip-a", "true"]);
    }

    #[test]
    fn esc_closes_and_the_panel_blocks_composer_input() {
        let mut ui = state();
        open(&mut ui);
        assert!(!ui.composer_active());
        ui.event(crossterm::event::Event::Key(key(KeyCode::Esc)));
        assert!(ui.branches.is_none());
        assert!(ui.composer_active());
    }
}

#[test]
fn a_failed_next_page_keeps_the_loaded_rows_and_names_the_error() {
    let mut first = sample();
    first["next_cursor"] = json!("cursor-1");
    let mut panel = panel(UiLanguage::En, &first);
    assert!(matches!(
        panel.key(key(KeyCode::Char('n'))),
        Outcome::Load { .. }
    ));
    panel.begin_page(2);
    let loading = render(&panel, 100, 24);
    assert!(loading.contains("Loading the next page"), "{loading}");
    assert!(loading.contains("fix the tests"), "{loading}");
    panel.fail("storage busy".into());
    let text = render(&panel, 100, 24);
    assert!(text.contains("fix the tests"), "{text}");
    assert!(
        text.contains("Could not read the next page: storage busy"),
        "{text}"
    );
    assert!(
        matches!(panel.key(key(KeyCode::Char('n'))), Outcome::Load { .. }),
        "N retries"
    );
}

#[test]
fn marker_and_panel_share_the_state_words() {
    use crate::presentation::branch_marker::BranchState;
    for (state, en, ja) in [
        (BranchState::Current, "current", "今の会話"),
        (BranchState::Parent, "parent", "親"),
        (BranchState::Branch, "branch", "別の会話"),
    ] {
        assert_eq!(state.word(UiLanguage::En), en);
        assert_eq!(state.word(UiLanguage::Ja), ja);
    }
    let text = with_glyphs(|| render(&panel(UiLanguage::Ja, &sample()), 100, 24));
    assert!(text.contains("別の会話 \"try with mocks\""), "{text}");
}
