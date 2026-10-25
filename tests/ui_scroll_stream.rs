//! Streaming attempt IDs are not ordinals or persisted conversation nodes.
use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;
use shellboy::{
    config::Config,
    presentation,
    ui::{UiAction, UiNode, UiState, UiUpdate, draw},
};
fn persisted(index: usize, text: String) -> UiNode {
    let payload =
        json!({"output":[{"type":"message","content":[{"type":"output_text","text":text}]}]});
    let mut projected = presentation::project("assistant_response", &payload, &text, 0);
    presentation::render_markdown(&mut projected, 76);
    UiNode {
        node_id: format!("node-{index}"),
        role: "assistant".into(),
        total_chars: text.chars().count(),
        text,
        base_char_offset: 0,
        reasoning_ranges: vec![],
        presentation: projected,
    }
}
#[test]
fn completed_prose_stream_cannot_remain_in_the_selected_path_ordinal_cache() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("fixture".into());
    let generation = ui.view().generation;
    let mut selected = (0..100)
        .map(|i| persisted(i, format!("SAVED {i:03}\n")))
        .collect::<Vec<_>>();
    ui.apply(UiUpdate::History {
        thread_id: "fixture".into(),
        generation,
        nodes: selected[50..].to_vec(),
        before_cursor: None,
        append: false,
        first_ordinal: 50,
        selection_revision: 100,
        total_nodes: 100,
    });
    let prose="# Completed response\n\nNormal prose **with Markdown**.\n\n```rust\nlet answer = 42;\n```\n".to_owned();
    ui.apply(UiUpdate::Stream {
        thread_id: "fixture".into(),
        generation,
        node_id: "stream-attempt-id".into(),
        role: "assistant".into(),
        text: prose.clone(),
        base_char_offset: 0,
        total_chars: prose.chars().count(),
        reasoning_ranges: vec![],
        presentation: Default::default(),
        ack: None,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    // The user stops following before the persisted final response arrives.
    ui.event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(!ui.view().follow);
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    let before = visible_history(&terminal);
    selected.push(persisted(100, prose.clone()));
    ui.apply(UiUpdate::StreamCommitted {
        thread_id: "fixture".into(),
        generation,
        attempt_id: "stream-attempt-id".into(),
        node_id: "node-100".into(),
    });
    assert!(ui.view().loading);
    let (view_revision, anchor) = ui
        .take_deferred_actions()
        .into_iter()
        .find_map(|action| match action {
            UiAction::RebaseHistory {
                view_revision,
                anchor,
                ..
            } => Some((view_revision, anchor)),
            UiAction::RebaseStream(request) => {
                assert_eq!(request.source.text, prose);
                Some((
                    request.view_revision,
                    shellboy::ui::ViewAnchor {
                        node_id: Some(request.node_id),
                        char_offset: request.anchor.char_offset,
                    },
                ))
            }
            _ => None,
        })
        .expect("completion must resolve real path ordinals without sending a message");
    assert_ne!(anchor.node_id.as_deref(), Some("stream-attempt-id"));
    ui.apply(UiUpdate::History {
        thread_id: "fixture".into(),
        generation,
        nodes: selected[51..].to_vec(),
        before_cursor: None,
        append: false,
        first_ordinal: 51,
        selection_revision: 101,
        total_nodes: 101,
    });
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert_eq!(
        visible_history(&terminal),
        before,
        "tail refresh must preserve the frame while rebasing"
    );
    let position = selected
        .iter()
        .position(|node| Some(&node.node_id) == anchor.node_id.as_ref())
        .unwrap();
    let first = position.saturating_sub(10);
    ui.apply(UiUpdate::HistoryWindow {
        thread_id: "fixture".into(),
        generation,
        view_revision,
        nodes: selected[first..selected.len().min(first + 50)].to_vec(),
        first_ordinal: first as u64,
        selection_revision: 101,
        total_nodes: 101,
        anchor,
    });
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert_eq!(
        first_saved_row(&visible_history(&terminal)),
        first_saved_row(&before),
        "canonical ordinal replacement must preserve the top saved prose row; completed Markdown may be rendered"
    );
    // One downward step after completion must not treat the provisional attempt
    // as canonical ordinal100 or manufacture a history.get attempt-node target.
    ui.event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(
        first_saved_row(&visible_history(&terminal)) >= first_saved_row(&before),
        "a downward step must not move the visible saved row upward"
    );
    let invalid = ui.view().nodes().iter().enumerate().find(|(offset, node)| {
        selected
            .get((ui.view().first_ordinal as usize) + offset)
            .is_none_or(|actual| actual.node_id != node.node_id)
    });
    assert!(
        invalid.is_none(),
        "completed stream corrupted the selected-path cache at {:?}; first={}, total={}",
        invalid.map(|(offset, node)| (offset, node.node_id.clone())),
        ui.view().first_ordinal,
        ui.view().total_nodes
    );
}

fn visible_history(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..22)
        .map(|row| {
            (0..80)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
        })
        .collect()
}
#[test]
fn paused_new_streams_do_not_enter_the_ordered_history_cache() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("fixture".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::History {
        thread_id: "fixture".into(),
        generation,
        nodes: vec![persisted(0, "saved prose\n".repeat(80))],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 1,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    ui.event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(!ui.view().follow);
    ui.take_deferred_actions();
    ui.apply(UiUpdate::Stream {
        thread_id: "fixture".into(),
        generation,
        node_id: "new-attempt".into(),
        role: "assistant".into(),
        text: "new streamed prose".into(),
        base_char_offset: 0,
        total_chars: 18,
        reasoning_ranges: vec![],
        presentation: Default::default(),
        ack: None,
    });
    assert_eq!(ui.view().nodes().len(), 1);
    assert_eq!(ui.view().nodes()[0].node_id, "node-0");
    for (thread, old_generation, attempt) in [
        ("other", generation, "new-attempt"),
        ("fixture", generation + 1, "new-attempt"),
        ("fixture", generation, "unknown-attempt"),
    ] {
        ui.apply(UiUpdate::StreamCommitted {
            thread_id: thread.into(),
            generation: old_generation,
            attempt_id: attempt.into(),
            node_id: "node-1".into(),
        });
    }
    assert!(!ui.view().loading);
    assert!(ui.take_deferred_actions().is_empty());
    assert_eq!(ui.view().nodes().len(), 1);
}

fn first_saved_row(lines: &[String]) -> usize {
    lines
        .iter()
        .find_map(|line| {
            line.split_once("SAVED ")
                .map(|(_, number)| number.trim().parse().unwrap())
        })
        .expect("visible saved prose row")
}

#[test]
fn late_commit_cannot_rename_a_different_thread_or_generation() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("fixture".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::History {
        thread_id: "fixture".into(),
        generation,
        nodes: vec![persisted(0, "saved prose\n".repeat(80))],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 1,
    });
    ui.apply(UiUpdate::Stream {
        thread_id: "fixture".into(),
        generation,
        node_id: "live-attempt".into(),
        role: "assistant".into(),
        text: "live text".into(),
        base_char_offset: 0,
        total_chars: 9,
        reasoning_ranges: vec![],
        presentation: Default::default(),
        ack: None,
    });
    let ids = ui
        .view()
        .nodes()
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    assert!(ids.iter().any(|id| id == "live-attempt"));
    ui.take_deferred_actions();
    for (thread, stale_generation) in [("other", generation), ("fixture", generation + 1)] {
        ui.apply(UiUpdate::StreamCommitted {
            thread_id: thread.into(),
            generation: stale_generation,
            attempt_id: "live-attempt".into(),
            node_id: "wrong-response".into(),
        });
    }
    assert_eq!(
        ui.view()
            .nodes()
            .iter()
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert!(ui.take_deferred_actions().is_empty());
    // The valid commit still works, proving stale replies did not consume the
    // provisional identity from the current view's tracking set.
    ui.apply(UiUpdate::StreamCommitted {
        thread_id: "fixture".into(),
        generation,
        attempt_id: "live-attempt".into(),
        node_id: "canonical-response".into(),
    });
    assert!(
        ui.view()
            .nodes()
            .iter()
            .all(|node| node.node_id != "canonical-response"),
        "canonical source is not fabricated from a temporary preview"
    );
    assert!(
        !ui.view()
            .nodes()
            .iter()
            .any(|node| node.node_id == "live-attempt")
    );
}

#[test]
fn completion_rebases_a_paused_anchor_inside_streamed_code() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("fixture".into());
    let generation = ui.view().generation;
    let saved = persisted(0, "earlier saved prose".into());
    ui.apply(UiUpdate::History {
        thread_id: "fixture".into(),
        generation,
        nodes: vec![saved.clone()],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 1,
    });
    let code = (0..100)
        .map(|line| format!("let line_{line:03} = {line};\n"))
        .collect::<String>();
    let answer = format!("# A long code answer\n\n```rust\n{code}```\n\nComplete.");
    ui.apply(UiUpdate::Stream {
        thread_id: "fixture".into(),
        generation,
        node_id: "code-attempt".into(),
        role: "assistant".into(),
        text: answer.clone(),
        base_char_offset: 0,
        total_chars: answer.chars().count(),
        reasoning_ranges: vec![],
        presentation: Default::default(),
        ack: None,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    ui.event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert_eq!(ui.view().anchor.node_id.as_deref(), Some("code-attempt"));
    let before = visible_history(&terminal);
    let first_code = |lines: &[String]| {
        lines
            .iter()
            .find_map(|line| {
                line.split_once("let line_")
                    .map(|(_, suffix)| suffix[..3].parse::<usize>().unwrap())
            })
            .unwrap()
    };
    let before_line = first_code(&before);
    ui.apply(UiUpdate::StreamCommitted {
        thread_id: "fixture".into(),
        generation,
        attempt_id: "code-attempt".into(),
        node_id: "node-1".into(),
    });
    let (view_revision, anchor) = ui
        .take_deferred_actions()
        .into_iter()
        .find_map(|action| match action {
            UiAction::RebaseHistory {
                view_revision,
                anchor,
                ..
            } => Some((view_revision, anchor)),
            UiAction::RebaseStream(request) => {
                assert_eq!(request.source.text, answer);
                Some((
                    request.view_revision,
                    shellboy::ui::ViewAnchor {
                        node_id: Some(request.node_id),
                        char_offset: request.anchor.char_offset,
                    },
                ))
            }
            _ => None,
        })
        .expect("provisional anchor needs a canonical path read");
    assert_eq!(anchor.node_id.as_deref(), Some("node-1"));
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert_eq!(visible_history(&terminal), before);
    ui.apply(UiUpdate::HistoryWindow {
        thread_id: "fixture".into(),
        generation,
        view_revision,
        nodes: vec![saved, persisted(1, answer)],
        first_ordinal: 0,
        selection_revision: 2,
        total_nodes: 2,
        anchor,
    });
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert_eq!(
        first_code(&visible_history(&terminal)),
        before_line,
        "Markdown completion must preserve the first visible code source line"
    );
    ui.event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert!(first_code(&visible_history(&terminal)) >= before_line);
    assert_eq!(ui.view().nodes()[1].node_id, "node-1");
}

#[tokio::test]
async fn dequeued_stale_stream_ack_is_returned_without_reviving_old_attempt() {
    use std::sync::Arc;
    use tokio::sync::Notify;
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("old".into());
    let generation = ui.view().generation;
    ui.select_thread("current".into());
    let ack = Arc::new(Notify::new());
    ui.apply(UiUpdate::Stream {
        thread_id: "old".into(),
        generation,
        node_id: "late".into(),
        role: "assistant".into(),
        text: "late text".into(),
        base_char_offset: 0,
        total_chars: 9,
        reasoning_ranges: vec![],
        presentation: Default::default(),
        ack: Some(ack.clone()),
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), ack.notified())
        .await
        .unwrap();
    assert!(ui.view().nodes().is_empty());
    ui.select_thread("old".into());
    assert!(ui.view().nodes().is_empty());
}
