//! Verify visible rows, independently of the scroll state's representation.
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use shellboy::{config::Config, ui::*};
use unicode_width::UnicodeWidthStr;

fn node(id: &str, text: String) -> UiNode {
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
fn fixture(nodes: Vec<UiNode>, width: u16, height: u16) -> (UiState, Terminal<TestBackend>) {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("scroll-test".into());
    ui.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: "scroll-test".into(),
        generation: ui.view().generation,
        total_nodes: nodes.len() as u64,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
    });
    (ui, Terminal::new(TestBackend::new(width, height)).unwrap())
}
fn rows(ui: &mut UiState, terminal: &mut Terminal<TestBackend>) -> Vec<String> {
    terminal.draw(|frame| draw(frame, ui)).unwrap();
    let buffer = terminal.backend().buffer();
    // Empty composer: one header, body with one-cell padding, three composer
    // rows, two footer rows. Compare only history cells, not changing status.
    (2..buffer.area.height - 6)
        .map(|y| {
            let mut row = String::new();
            let mut x = 1;
            while x < buffer.area.width - 1 {
                let symbol = buffer[(x, y)].symbol();
                row.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            row
        })
        .collect()
}
fn wheel(ui: &mut UiState, kind: MouseEventKind) {
    ui.event(Event::Mouse(MouseEvent {
        kind,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
}
fn key(ui: &mut UiState, code: KeyCode) {
    ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

#[test]
fn wheel_moves_three_visible_rows_and_inverse_restores_the_full_last_page() {
    for width in [40, 100] {
        for text in [
            (1..=200).map(|i| format!("R{i:03}\n")).collect::<String>(),
            (1..=100)
                .map(|i| {
                    format!(
                        "R{i:03} 日本語と結合文字e\u{301}、絵文字🙂を含む長い文章の折り返しです。 "
                    )
                })
                .collect::<String>(),
        ] {
            let (mut ui, mut terminal) = fixture(vec![node("long", text)], width, 28);
            let before = rows(&mut ui, &mut terminal);
            wheel(&mut ui, MouseEventKind::ScrollUp);
            let up = rows(&mut ui, &mut terminal);
            assert_eq!(&up[3..], &before[..before.len() - 3], "width={width}");
            assert!(!ui.view().follow);
            wheel(&mut ui, MouseEventKind::ScrollDown);
            assert_eq!(rows(&mut ui, &mut terminal), before);
            assert!(ui.view().follow);
        }
    }
}

#[test]
fn page_up_uses_viewport_height_and_keeps_one_row_of_overlap() {
    for height in [20, 36] {
        let text = (1..=200).map(|i| format!("R{i:03}\n")).collect();
        let (mut ui, mut terminal) = fixture(vec![node("long", text)], 60, height);
        let before = rows(&mut ui, &mut terminal);
        key(&mut ui, KeyCode::PageUp);
        let up = rows(&mut ui, &mut terminal);
        assert_eq!(up.last(), before.first());
        key(&mut ui, KeyCode::PageDown);
        assert_eq!(rows(&mut ui, &mut terminal), before);
    }
}

#[test]
fn return_to_tail_then_wheel_uses_current_screen_instead_of_old_node_anchor() {
    let nodes = (1..=100)
        .map(|i| node(&format!("n-{i}"), format!("M{i:03}")))
        .collect();
    let (mut ui, mut terminal) = fixture(nodes, 60, 28);
    let tail = rows(&mut ui, &mut terminal);
    for _ in 0..5 {
        wheel(&mut ui, MouseEventKind::ScrollUp);
    }
    rows(&mut ui, &mut terminal);
    ui.event(Event::Key(KeyEvent::new(
        KeyCode::End,
        KeyModifiers::CONTROL,
    )));
    assert_eq!(rows(&mut ui, &mut terminal), tail);
    wheel(&mut ui, MouseEventKind::ScrollUp);
    let up = rows(&mut ui, &mut terminal);
    assert_eq!(&up[3..], &tail[..tail.len() - 3]);
}

#[test]
fn overlays_and_composer_wheel_do_not_move_hidden_history() {
    for overlay in [KeyCode::F(1), KeyCode::F(6), KeyCode::F(7), KeyCode::F(8)] {
        let text = (1..=200).map(|i| format!("R{i:03}\n")).collect();
        let (mut ui, mut terminal) = fixture(vec![node("long", text)], 60, 28);
        let before = rows(&mut ui, &mut terminal);
        key(&mut ui, overlay);
        rows(&mut ui, &mut terminal);
        wheel(&mut ui, MouseEventKind::ScrollUp);
        key(&mut ui, KeyCode::Esc);
        key(&mut ui, KeyCode::Esc);
        assert_eq!(rows(&mut ui, &mut terminal), before, "{overlay:?}");
        assert!(ui.view().follow);
        ui.event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 25,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(rows(&mut ui, &mut terminal), before);
    }
}

#[test]
fn burst_wheel_and_new_output_preserve_the_rows_being_read() {
    let original: String = (1..=200).map(|i| format!("R{i:03}\n")).collect();
    let (mut ui, mut terminal) = fixture(vec![node("long", original.clone())], 60, 28);
    let tail = rows(&mut ui, &mut terminal);
    for _ in 0..10 {
        wheel(&mut ui, MouseEventKind::ScrollUp);
    }
    let pinned = rows(&mut ui, &mut terminal);
    assert_ne!(pinned, tail);
    let updated = format!("{original}NEW_OUTPUT\n");
    ui.apply(UiUpdate::Stream {
        thread_id: "scroll-test".into(),
        generation: ui.view().generation,
        node_id: "long".into(),
        role: "assistant".into(),
        total_chars: updated.chars().count(),
        text: updated,
        base_char_offset: 0,
        reasoning_ranges: vec![],
        presentation: Default::default(),
        ack: None,
    });
    assert_eq!(rows(&mut ui, &mut terminal), pinned);
    assert!(ui.view().unseen);
    assert!(!ui.view().follow);
}

#[test]
fn uncached_neighbor_is_loaded_before_paging_can_skip_its_unknown_height() {
    let mut nodes: Vec<_> = (0..10)
        .map(|i| {
            node(
                &format!("n-{i}"),
                (0..100)
                    .map(|line| format!("N{i} row {line:03}\n"))
                    .collect(),
            )
        })
        .collect();
    // Body eviction leaves the original length and identity but no known row
    // count. Treating its placeholder as its real height skips the message.
    let complete = nodes.clone();
    nodes[4].text.clear();
    let (mut ui, mut terminal) = fixture(nodes, 60, 28);
    ui.apply(UiUpdate::ViewState {
        thread_id: "scroll-test".into(),
        generation: ui.view().generation,
        view_revision: ui.view().view_revision,
        anchor: ViewAnchor {
            node_id: Some("n-5".into()),
            char_offset: 0,
        },
        follow: false,
    });
    rows(&mut ui, &mut terminal);
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::PageUp,
        KeyModifiers::NONE,
    )));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, UiAction::LoadNodeWindow { node_id, .. } if node_id == "n-4")),
        "load the immediate neighbor before estimating a page through it: {actions:?}"
    );
    ui.apply(UiUpdate::NodeWindow {
        thread_id: "scroll-test".into(),
        generation: ui.view().generation,
        view_revision: ui.view().view_revision,
        node: complete[4].clone(),
    });
    let (mut reference, mut reference_terminal) = fixture(complete, 60, 28);
    reference.apply(UiUpdate::ViewState {
        thread_id: "scroll-test".into(),
        generation: reference.view().generation,
        view_revision: reference.view().view_revision,
        anchor: ViewAnchor {
            node_id: Some("n-5".into()),
            char_offset: 0,
        },
        follow: false,
    });
    rows(&mut reference, &mut reference_terminal);
    key(&mut reference, KeyCode::PageUp);
    assert_eq!(
        rows(&mut ui, &mut terminal),
        rows(&mut reference, &mut reference_terminal)
    );
}
