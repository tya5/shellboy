use super::*;
use ratatui::{Terminal, backend::TestBackend};

fn ui() -> UiState {
    ui_in(crate::config::UiLanguage::En)
}
fn ui_in(language: crate::config::UiLanguage) -> UiState {
    let mut config = Config::default();
    config.ui.language = language;
    let mut ui = UiState::new(&config, std::env::temp_dir());
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
        awaiting_reply: Vec::new(),
        items: if admitted {
            vec![]
        } else {
            vec![PendingMessage {
                expect_reply: false,
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
    footer_at(ui, 100)
}
fn footer_at(ui: &UiState, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
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
        while x < width {
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
fn capacity_wait_keeps_the_existing_accepting_status() {
    let mut ui = ui_in(crate::config::UiLanguage::Ja);
    let (_, revision, _) = send(&mut ui, "waiting for receipt capacity");
    let start = Instant::now();
    for seconds in [0, 5, 30] {
        ui.animation_tick(start + std::time::Duration::from_secs(seconds));
        assert_eq!(
            ui.view().pending[&revision].status,
            ReceiptStatus::Accepting
        );
        assert!(footer(&ui).contains("送信受付中"));
    }
}

#[test]
fn enter_draws_and_animates_before_frontend_then_hands_off_only_after_matching_history() {
    let mut ui = ui_in(crate::config::UiLanguage::Ja);
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
    let mut ui = ui_in(crate::config::UiLanguage::Ja);
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

#[test]
fn send_progress_does_not_become_a_stale_operation_notice() {
    let mut ui = ui_in(crate::config::UiLanguage::Ja);
    ui.insert("synthetic request", false);
    let id = ui.view().draft.draft_id.clone();
    let revision = ui.view().draft.revision;
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, UiAction::Send { .. }))
    );
    assert!(ui.view().operation_notice.is_none());
    assert!(footer(&ui).contains("送信受付中"));
    receipt(&mut ui, &id, revision, ReceiptStatus::Accepted);
    source(&mut ui, &id, revision, "m1");
    history(&mut ui, "m1", 1);
    receipt(&mut ui, &id, revision, ReceiptStatus::Completed);
    let generation = ui.view().generation;
    ui.apply(UiUpdate::Activity {
        thread_id: "t".into(),
        generation,
        epoch: 1,
        observed_at: Instant::now(),
        snapshot: activity::ActivitySnapshot {
            phase: activity::ActivityPhase::Completed,
            ..Default::default()
        },
    });
    ui.event(Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
    assert!(
        ui.view().operation_notice.is_none(),
        "navigation cannot announce an old receipt again"
    );
    assert!(!footer(&ui).contains("送信要求を受け付け中"));
}
#[test]
fn history_failure_remains_visible_in_quiet_footer() {
    let mut ui = ui();
    ui.select_thread("t".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::LoadFailed {
        thread_id: "t".into(),
        generation,
        view_revision: None,
        message: "synthetic history read failed".into(),
    });
    assert_eq!(
        ui.view().operation_notice.as_ref().unwrap().kind,
        history_interaction::NoticeKind::Failure
    );
    assert!(footer(&ui).contains("synthetic history read failed"));
    ui.apply(UiUpdate::Status {
        thread_id: Some("t".into()),
        text: "background status".into(),
    });
    assert!(footer(&ui).contains("synthetic history read failed"));
}
#[test]
fn unknown_activity_is_not_displayed_as_known_idle() {
    let mut ui = ui_in(crate::config::UiLanguage::Ja);
    ui.select_thread("t".into());
    assert!(footer(&ui).contains("状態を確認中"));
    assert!(!footer(&ui).contains("入力できます"));
    let generation = ui.view().generation;
    ui.apply(UiUpdate::Activity {
        thread_id: "t".into(),
        generation,
        epoch: 1,
        observed_at: Instant::now(),
        snapshot: activity::ActivitySnapshot {
            phase: activity::ActivityPhase::Idle,
            ..Default::default()
        },
    });
    assert!(footer(&ui).contains("入力できます"));
    assert!(!footer(&ui).contains("状態を確認中"));
}

#[test]
fn noop_delete_of_next_input_during_send_preserves_revision_after_commit() {
    for (key, at_start) in [(KeyCode::Delete, false), (KeyCode::Backspace, true)] {
        let mut ui = ui();
        let (id, revision, _) = send(&mut ui, "synthetic queued input");
        let next_revision = ui.view().draft.revision;
        if at_start {
            ui.active_view_mut().draft.cursor = 0;
        }
        ui.event(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)));
        assert_eq!(ui.view().draft.revision, next_revision);
        assert!(ui.view().draft.text.is_empty());
        saved(&mut ui, &id, revision, "m1", 1, false);
        assert_eq!(ui.view().pending_messages.total, 1);
        assert!(ui.view().draft.text.is_empty());
        assert!(ui.submit().is_empty());
    }
}

#[test]
fn effective_delete_during_send_preserves_user_edit_after_commit() {
    let mut ui = ui();
    let (id, revision, _) = send(&mut ui, "synthetic queued input");
    ui.insert("next input", false);
    ui.event(Event::Key(KeyEvent::new(
        KeyCode::Backspace,
        KeyModifiers::NONE,
    )));
    assert!(ui.view().draft.revision > revision);
    saved(&mut ui, &id, revision, "m1", 1, false);
    assert_eq!(ui.view().draft.text, "next inpu");
}

#[test]
fn submitting_owns_a_frozen_copy_and_composer_accepts_only_the_next_input() {
    for language in [crate::config::UiLanguage::En, crate::config::UiLanguage::Ja] {
        let mut ui = ui_in(language);
        let (id, revision, _) = send(&mut ui, "submitted copy");
        assert!(ui.view().draft.text.is_empty());
        assert_eq!(ui.view().pending[&revision].snapshot.text, "submitted copy");
        assert_eq!(
            ui.view().pending[&revision].status,
            ReceiptStatus::Accepting
        );
        assert!(
            ui.submit().is_empty(),
            "Enter cannot resubmit the pending copy"
        );
        ui.insert("next input", false);
        let next = ui.view().draft.clone();
        saved(&mut ui, &id, revision, "saved", 1, false);
        assert_eq!(
            ui.view().draft,
            next,
            "late commit cannot clear new editing"
        );
    }
}

#[test]
fn failed_submissions_restore_only_an_unchanged_composer_including_attachment_metadata() {
    for edited in [false, true] {
        let mut ui = ui();
        ui.insert("original", false);
        ui.apply(UiUpdate::ClipboardAttachment {
            draft_id: ui.view().draft.draft_id.clone(),
            revision: ui.view().draft.revision,
            attachment: UiAttachment {
                id: "attachment".into(),
                kind: "file".into(),
                path: "/synthetic".into(),
                state: "ready".into(),
                position: Some(8),
            },
        });
        let original = ui.view().draft.clone();
        let action = ui.submit().remove(0);
        assert!(ui.view().draft.attachments.is_empty());
        if edited {
            ui.insert("newer!", false);
            ui.delete(true);
            assert!(ui.take_deferred_actions().iter().any(|action| matches!(action,
                UiAction::DraftAttachments { attachment_ids, .. } if attachment_ids == &["attachment".to_owned()]
            )), "editing the next input cannot unpin the pending snapshot's image");
        }
        ui.action_rejected(&action);
        assert_eq!(
            ui.view().draft.text,
            if edited { "newer" } else { &original.text }
        );
        assert_eq!(ui.view().pending[&original.revision].snapshot, original);
        if !edited {
            assert_eq!(ui.view().draft.attachments, original.attachments);
        }
        assert!(ui.view().sending().is_none());
        let restored = ui.view().draft.clone();
        ui.action_rejected(&action);
        assert_eq!(
            ui.view().draft,
            restored,
            "duplicate failure must not restore twice"
        );
    }
}

#[test]
fn long_unchanged_db_debug_item_cannot_hide_submitting_or_later_state_updates() {
    use crate::storage::{ActiveDbJob, DbJobLane, DbWorkerActivity};
    for language in [crate::config::UiLanguage::En, crate::config::UiLanguage::Ja] {
        let mut ui = ui_in(language);
        let _ = send(&mut ui, "queued while DB is busy");
        let started = Instant::now();
        ui.apply(UiUpdate::DbWorkerActivity(DbWorkerActivity {
            active: Some(ActiveDbJob {
                id: 1,
                kind: "control::db::execute_kind::history_list",
                lane: DbJobLane::Foreground,
                started_at: std::time::SystemTime::now(),
                started,
            }),
            no_progress: vec![],
        }));
        ui.animation_tick(started + Duration::from_secs(227));
        let text = footer(&ui);
        assert!(
            text.contains(if language == crate::config::UiLanguage::En {
                "Accepting input"
            } else {
                "送信受付中"
            }),
            "{text}"
        );
        assert!(
            text.contains("DB control::db::execute kind::history list"),
            "debug wording is unchanged: {text}"
        );
        ui.apply(UiUpdate::Status {
            thread_id: None,
            text: "live status update".into(),
        });
        assert!(footer(&ui).contains("live status update"));
        ui.insert("next input", false);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(screen.contains("next input"));
        assert_eq!(
            ui.view().pending.values().next().unwrap().status,
            ReceiptStatus::Accepting
        );
    }
}

#[test]
fn durable_commit_after_observed_failure_clears_only_untouched_automatic_restore() {
    for edited in [false, true] {
        let mut ui = ui();
        let (id, revision, action) = send(&mut ui, "original request");
        ui.action_rejected(&action);
        assert_eq!(ui.view().draft.text, "original request");
        if edited {
            ui.insert(" changed", false);
        }
        saved(&mut ui, &id, revision, "saved-after-error", 1, false);
        assert_eq!(
            ui.view().draft.text,
            if edited {
                "original request changed"
            } else {
                ""
            }
        );
        assert!(!ui.view().pending.contains_key(&revision));
    }
}

#[test]
fn edited_send_uses_the_same_local_handoff_and_failure_restore() {
    let mut ui = ui();
    ui.insert("edited request", false);
    let target = EditRequestTarget {
        thread_id: "t".into(),
        node_id: "original".into(),
        run_id: None,
    };
    ui.view_mut().draft.edit = Some(target.clone());
    let revision = ui.view().draft.revision;
    let action = ui.submit().remove(0);
    assert!(
        matches!(&action, UiAction::SendEdited { source_node_id, .. } if source_node_id == "original")
    );
    assert!(ui.view().draft.text.is_empty());
    assert!(ui.view().draft.edit.is_none());
    assert_eq!(
        ui.view().pending[&revision].snapshot.edit,
        Some(target.clone())
    );
    ui.action_rejected(&action);
    assert_eq!(ui.view().draft.text, "edited request");
    assert_eq!(ui.view().draft.edit, Some(target));
}

#[test]
fn narrow_footer_preserves_primary_state_ahead_of_any_long_db_job() {
    for language in [crate::config::UiLanguage::En, crate::config::UiLanguage::Ja] {
        let mut ui = ui_in(language);
        let _ = send(&mut ui, "pending input");
        for width in [24, 40, 60] {
            ui.db_status = Some("DB control::db::execute kind::history list 3m47s".into());
            let text = footer_at(&ui, width);
            assert!(
                text.contains(if language == crate::config::UiLanguage::En {
                    "Accepting input"
                } else {
                    "送信受付中"
                }),
                "width {width}: {text}"
            );
            assert!(text.contains("DB"), "width {width}: {text}");
            assert_eq!(text.lines().count(), 2);
        }
    }
}

#[test]
fn autosave_and_thread_switch_own_next_draft_not_the_pending_request() {
    let mut ui = ui();
    let _ = ui.select_thread("t".into());
    let (_, revision, action) = send(&mut ui, "pending request");
    for request in ui.persist_active() {
        assert!(
            matches!(request, UiAction::SaveDraft { draft, .. } if draft.text.is_empty() && draft.revision > revision)
        );
    }
    ui.insert("new input", false);
    let key = ui.active.clone();
    let actions = ui.select_thread("other".into());
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, UiAction::SaveDraft { draft, .. } if draft.text == "new input"))
    );
    ui.insert("other thread input", false);
    ui.action_rejected(&action);
    assert_eq!(ui.view().draft.text, "other thread input");
    assert_eq!(ui.views[&key].draft.text, "new input");
    assert_eq!(
        ui.views[&key].pending[&revision].snapshot.text,
        "pending request"
    );
    assert!(matches!(
        ui.views[&key].pending[&revision].status,
        ReceiptStatus::Failed
    ));
}

#[test]
fn delayed_edit_confirmation_restores_only_the_untouched_composer() {
    for edited in [false, true] {
        let mut ui = ui();
        ui.insert("edited original", false);
        ui.view_mut().draft.edit = Some(EditRequestTarget {
            thread_id: "old".into(),
            node_id: "original".into(),
            run_id: None,
        });
        let draft_id = ui.view().draft.draft_id.clone();
        let revision = ui.view().draft.revision;
        let _ = ui.submit();
        if edited {
            ui.insert("next input", false);
        }
        ui.apply(UiUpdate::EditConfirmationRequired {
            draft_id,
            revision,
            run_id: "new-run".into(),
        });
        if edited {
            assert_eq!(ui.view().draft.text, "next input");
            assert!(ui.confirmation.is_none());
        } else {
            assert_eq!(ui.view().draft.text, "edited original");
            assert!(
                matches!(&ui.confirmation, Some(Confirmation::EditStop { revision: restored, .. }) if *restored == ui.view().draft.revision && *restored > revision)
            );
        }
        assert_eq!(ui.view().pending[&revision].status, ReceiptStatus::Failed);
    }
}

fn reply_target(id: &str) -> ReplyTarget {
    ReplyTarget {
        message_id: id.into(),
        sender: "fixture sender".into(),
        created_at: "now".into(),
        excerpt: "original request".into(),
        applied: true,
    }
}
fn reply_action(ui: &mut UiState, text: &str) -> CommandSubmission {
    ui.insert(text, false);
    let mut actions = ui.submit();
    let UiAction::SubmittedCommand { submission, action } = actions.remove(0) else {
        panic!("reply submission required")
    };
    assert!(matches!(
        *action,
        UiAction::Command(Command::MessageReply(_))
    ));
    submission
}
#[test]
fn reply_submit_is_text_only_preserves_slash_text_and_restores_failed_draft() {
    let mut ui = ui();
    ui.active_view_mut().composer.reply = Some(reply_target("original"));
    let submission = reply_action(&mut ui, "/literal reply");
    assert!(ui.view().draft.text.is_empty());
    assert!(ui.submit().is_empty());
    ui.command_outcome(submission, Err("synthetic cancelled".into()));
    assert_eq!(ui.view().draft.text, "/literal reply");
    assert_eq!(
        ui.view().composer.reply.as_ref().unwrap().message_id,
        "original"
    );
}
#[test]
fn reply_ack_does_not_clear_a_newer_target_or_draft() {
    let mut ui = ui();
    ui.active_view_mut().composer.reply = Some(reply_target("old"));
    let submission = reply_action(&mut ui, "reply");
    ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    ui.active_view_mut().composer.reply = Some(reply_target("new"));
    ui.insert("new draft", false);
    ui.command_outcome(submission, Ok(()));
    assert_eq!(ui.view().draft.text, "new draft");
    assert_eq!(ui.view().composer.reply.as_ref().unwrap().message_id, "new");
}
#[test]
fn reply_mode_is_per_view_and_escape_preserves_draft() {
    let mut ui = ui();
    ui.select_thread("one".into());
    ui.active_view_mut().composer.reply = Some(reply_target("original"));
    ui.insert("draft", false);
    ui.select_thread("two".into());
    assert!(ui.view().composer.reply.is_none());
    ui.select_thread("one".into());
    assert!(ui.view().composer.reply.is_some());
    ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(ui.view().composer.reply.is_none());
    assert_eq!(ui.view().draft.text, "draft");
}
#[test]
fn replies_popup_selects_only_applied_requests_and_draws_both_languages() {
    for language in [crate::config::UiLanguage::Ja, crate::config::UiLanguage::En] {
        let mut ui = ui_in(language);
        let mut target = reply_target("original");
        target.applied = false;
        ui.active_view_mut()
            .pending_messages
            .awaiting_reply
            .push(target);
        ui.queue_popup.replies = true;
        ui.queue_popup.visible = true;
        ui.queue_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(ui.view().composer.reply.is_none());
        ui.active_view_mut().pending_messages.awaiting_reply[0].applied = true;
        for width in [24, 80] {
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
            terminal
                .draw(|f| {
                    ui.queue_popup
                        .draw(f, f.area(), &ui.view().pending_messages, language)
                })
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("/replies"), "{text}");
        }
        ui.queue_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            ui.view().composer.reply.as_ref().unwrap().message_id,
            "original"
        );
        assert!(!ui.queue_popup.visible);
        let caption = ui.view().composer.reply.as_ref().unwrap().caption(language);
        assert!(
            caption.contains(if language == crate::config::UiLanguage::Ja {
                "返信先"
            } else {
                "Replying to"
            })
        );
    }
}

#[test]
fn reply_local_handoff_keeps_new_input_without_a_db_ack_for_success_or_failure() {
    for language in [crate::config::UiLanguage::En, crate::config::UiLanguage::Ja] {
        for success in [false, true] {
            let mut ui = ui_in(language);
            ui.active_view_mut().composer.reply = Some(reply_target("original"));
            let submission = reply_action(&mut ui, "frozen reply");
            let pending = &ui.view().pending[&submission.revision];
            assert_eq!(pending.snapshot.text, "frozen reply");
            assert_eq!(pending.status, ReceiptStatus::Accepting);
            assert_eq!(pending.cleared_draft.as_ref(), Some(&ui.view().draft));
            assert!(pending.restored_revision.is_none());
            assert!(ui.view().draft.text.is_empty());
            assert!(ui.view().draft.revision > submission.revision);
            ui.insert("next input", false);
            assert!(
                ui.submit().is_empty(),
                "cannot submit a second reply before the first completes"
            );
            let next = ui.view().draft.clone();
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
            let screen = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(screen.contains("next input"));
            ui.command_outcome(
                submission.clone(),
                if success {
                    Ok(())
                } else {
                    Err("synthetic failure".into())
                },
            );
            assert_eq!(
                ui.view().draft,
                next,
                "late reply result must not overwrite newer input"
            );
            if success {
                assert!(!ui.view().pending.contains_key(&submission.revision));
            } else {
                assert_eq!(
                    ui.view().pending[&submission.revision].snapshot.text,
                    "frozen reply"
                );
                assert_eq!(
                    ui.view().pending[&submission.revision].status,
                    ReceiptStatus::Failed
                );
            }
        }
    }
}
