use super::*;
use ratatui::{Terminal, backend::TestBackend};

fn ui() -> UiState {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.apply(UiUpdate::Initialize {
        cwd: std::env::temp_dir(),
    });
    ui
}
fn send(ui: &mut UiState, text: &str) -> (String, u64, UiAction) {
    ui.insert(text, false);
    let key = (ui.view().draft.draft_id.clone(), ui.view().draft.revision);
    let action = ui.submit().remove(0);
    assert!(matches!(action, UiAction::Send { .. }));
    (key.0, key.1, action)
}
fn receipt(ui: &mut UiState, id: &str, revision: u64, status: ReceiptStatus) {
    ui.apply(UiUpdate::Receipt {
        draft_id: id.into(),
        revision,
        thread_id: Some("t".into()),
        status,
        message: None,
    });
}
fn source(ui: &mut UiState, id: &str, revision: u64, node: &str) {
    ui.apply(UiUpdate::ReceiptSource {
        draft_id: id.into(),
        revision,
        node_id: node.into(),
    });
}
fn history(ui: &mut UiState, node: &str, revision: u64) {
    ui.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: revision,
        thread_id: "t".into(),
        generation: ui.view().generation,
        nodes: vec![UiNode {
            node_id: node.into(),
            role: "user".into(),
            text: "saved request".into(),
            ..Default::default()
        }],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: revision,
        total_nodes: 1,
    });
}
fn saved(
    ui: &mut UiState,
    id: &str,
    revision: u64,
    node: &str,
    read_revision: u64,
    admitted: bool,
) {
    let pending = PendingMessages {
        items: if admitted {
            vec![]
        } else {
            vec![PendingMessage {
                message_id: node.into(),
                operation_id: "operation".into(),
                sender_thread: None,
                created_at: "2026-10-30T00:00:00Z".into(),
                held: false,
                hold_reason: None,
                uncertain: false,
                now: false,
                excerpt: "saved request".into(),
                excerpt_truncated: false,
                attachment_count: 0,
            }]
        },
        total: u64::from(!admitted),
        submitted: vec![SubmittedMessage {
            operation_id: "operation".into(),
            source_draft_id: id.into(),
            revision,
            message_id: node.into(),
        }],
    };
    let generation = ui.view().generation;
    let history = UiUpdate::History {
        thread_id: "t".into(),
        generation,
        nodes: if admitted {
            vec![UiNode {
                node_id: node.into(),
                role: "user".into(),
                text: "saved request".into(),
                ..Default::default()
            }]
        } else {
            vec![]
        },
        pending,
        read_revision,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: u64::from(admitted),
        total_nodes: u64::from(admitted),
    };
    ui.apply(UiUpdate::SavedMessage {
        draft_id: id.into(),
        revision,
        generation,
        thread_id: "t".into(),
        status: ReceiptStatus::Accepted,
        history: Box::new(history),
    });
}
fn footer(ui: &UiState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 2)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            draw_status(frame, area, ui)
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..2 {
        let mut x = 0;
        while x < 100 {
            let symbol = buffer[(x, y)].symbol();
            text.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        text.push('\n');
    }
    text
}
fn preparing(ui: &mut UiState) {
    ui.apply(UiUpdate::Activity {
        thread_id: "t".into(),
        generation: ui.view().generation,
        epoch: 1,
        snapshot: activity::ActivitySnapshot {
            phase: activity::ActivityPhase::Preparing,
            run_id: Some("r".into()),
            turn_id: Some("u".into()),
            turn_in_progress: true,
            ..Default::default()
        },
        observed_at: Instant::now(),
    });
}

#[test]
fn enter_draws_and_animates_before_frontend_then_hands_off_only_after_matching_history() {
    let mut ui = ui();
    let (id, rev, _) = send(&mut ui, "first request");
    assert!(ui.dirty);
    assert!(footer(&ui).contains("送信受付中"));
    assert!(!footer(&ui).contains("入力できます"));
    ui.dirty = false;
    ui.animation_tick(Instant::now());
    assert!(
        ui.dirty,
        "local send uses the existing animation clock before a Run exists"
    );
    ui.apply(UiUpdate::SendOperation {
        draft_id: id.clone(),
        revision: rev,
        operation_id: "operation".into(),
    });
    saved(&mut ui, &id, rev, "m1", 1, false);
    preparing(&mut ui);
    assert!(footer(&ui).contains("準備中"));
    assert!(!footer(&ui).contains("保存済み・取得中"));
    assert!(
        ui.view().pending.is_empty(),
        "the DB projection owns committed input"
    );
    assert_eq!(ui.view().pending_messages.items[0].message_id, "m1");
    assert!(
        ui.view().nodes().is_empty(),
        "queued input has not entered the selected path"
    );
    source(&mut ui, &id, rev, "m1");
    receipt(&mut ui, &id, rev, ReceiptStatus::Accepting);
    assert!(
        ui.view().pending.is_empty(),
        "late progress/source replies cannot recreate local copies"
    );
    history(&mut ui, "m1", 2);
    assert_eq!(ui.view().nodes()[0].node_id, "m1");
    assert_eq!(
        ui.view().pending_messages.total,
        0,
        "one paired snapshot moves input out of the queue"
    );
    assert!(footer(&ui).contains("準備中"));
    history(&mut ui, "another-page", 3);
    assert!(
        ui.view().sending().is_none(),
        "scroll/cache changes cannot recreate a local saved-message clock"
    );
}

#[test]
fn fast_completion_and_history_before_source_do_not_drop_the_message_binding() {
    let mut ui = ui();
    let (id, rev, _) = send(&mut ui, "fast");
    saved(&mut ui, &id, rev, "m", 1, true);
    assert!(
        ui.view().pending.is_empty(),
        "saved history carries the immutable message binding before a source callback"
    );
    assert_eq!(ui.view().nodes()[0].node_id, "m");
    assert_eq!(ui.view().pending_messages.total, 0);
    receipt(&mut ui, &id, rev, ReceiptStatus::Completed);
    history(&mut ui, "stale-page", 0);
    source(&mut ui, &id, rev, "m");
    assert_eq!(
        ui.view().nodes()[0].node_id,
        "m",
        "an older history page must not replace the paired commit"
    );
    assert!(ui.view().pending.is_empty());
    assert!(ui.view().sending().is_none());
    assert!(!footer(&ui).contains("保存済み・取得中"));
}

#[test]
fn sends_keep_separate_revisions_and_failed_admission_stops_motion_without_clearing_input() {
    let mut ui = ui();
    let (id, first, _) = send(&mut ui, "first");
    receipt(&mut ui, &id, first, ReceiptStatus::Accepted);
    let (_, second, action) = send(&mut ui, "second");
    assert_ne!(first, second);
    assert_eq!(ui.view().sending().unwrap().1, 2);
    source(&mut ui, &id, first, "m1");
    history(&mut ui, "m1", 1);
    assert_eq!(ui.view().sending().unwrap().1, 1);
    assert_eq!(ui.view().pending[&second].status, ReceiptStatus::Accepting);
    ui.action_rejected(&action);
    assert!(ui.view().sending().is_none());
    assert_eq!(ui.view().draft.text, "second");
    assert_eq!(ui.view().pending[&second].status, ReceiptStatus::Failed);
    let (_, third, _) = send(&mut ui, " again");
    assert!(
        third > second,
        "explicit retry/new send is a separate UI request"
    );
}

#[test]
fn switching_threads_does_not_show_another_threads_pending_send() {
    let mut ui = ui();
    let (id, rev, _) = send(&mut ui, "first");
    ui.new_chat();
    assert!(ui.view().sending().is_none());
    receipt(&mut ui, &id, rev, ReceiptStatus::Accepted);
    assert!(ui.view().sending().is_none());
    assert!(!footer(&ui).contains("表示待ち"));
}

#[test]
fn older_completed_sends_outside_the_history_window_do_not_leave_stale_progress() {
    let mut ui = ui();
    let (id, first, _) = send(&mut ui, "first");
    receipt(&mut ui, &id, first, ReceiptStatus::Accepted);
    source(&mut ui, &id, first, "m1");
    let (_, second, _) = send(&mut ui, "second");
    receipt(&mut ui, &id, second, ReceiptStatus::Accepted);
    source(&mut ui, &id, second, "m2");
    history(&mut ui, "m2", 1);
    receipt(&mut ui, &id, first, ReceiptStatus::Completed);
    receipt(&mut ui, &id, second, ReceiptStatus::Completed);
    assert!(ui.view().sending().is_none());
}

#[test]
fn completed_send_with_a_bound_source_outside_the_history_window_is_no_longer_busy() {
    let mut ui = ui();
    let (id, rev, _) = send(&mut ui, "request outside the current window");
    receipt(&mut ui, &id, rev, ReceiptStatus::Accepted);
    source(&mut ui, &id, rev, "m1");
    history(&mut ui, "later-tool-output", 1);
    receipt(&mut ui, &id, rev, ReceiptStatus::Completed);
    assert!(ui.view().sending().is_none());
}

#[test]
fn recovery_stays_static_when_a_new_send_is_pending() {
    let mut ui = ui();
    let _ = send(&mut ui, "resume request");
    ui.active_view_mut().activity.snapshot = Some(activity::ActivitySnapshot {
        phase: activity::ActivityPhase::Recovery,
        ..Default::default()
    });
    assert!(footer(&ui).contains("復旧待ち"));
    assert!(footer(&ui).contains("送信受付中"));
    ui.animation_tick(Instant::now());
    ui.dirty = false;
    ui.animation_tick(Instant::now() + std::time::Duration::from_secs(1));
    assert!(
        !ui.dirty,
        "a pending send does not animate a static critical phase"
    );
}

#[test]
fn terminal_admission_is_not_answered_or_cancelled_and_keeps_late_source_binding() {
    let mut ui = ui();
    let (id, rev, _) = send(&mut ui, "interrupted after admission");
    receipt(&mut ui, &id, rev, ReceiptStatus::Accepted);
    receipt(&mut ui, &id, rev, ReceiptStatus::Applied);
    assert!(ui.view().status.contains("会話へ反映済み"));
    assert!(!ui.view().status.contains("完了"));
    assert!(!ui.view().status.contains("取消"));
    assert!(!ui.view().status.contains("/restore"));
    assert!(ui.view().sending().is_none());
    assert!(ui.view().pending.contains_key(&rev));
    receipt(&mut ui, &id, rev, ReceiptStatus::Cancelled);
    assert_eq!(ui.view().pending[&rev].status, ReceiptStatus::Applied);
    source(&mut ui, &id, rev, "original-message");
    assert!(!ui.view().pending.contains_key(&rev));
}
