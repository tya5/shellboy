//! Current saved input, separate from immutable selected conversation history.
use super::*;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingMessages {
    pub items: Vec<PendingMessage>,
    pub total: u64,
    pub submitted: Vec<SubmittedMessage>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmittedMessage {
    pub operation_id: String,
    pub source_draft_id: String,
    pub revision: u64,
    pub message_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingMessage {
    pub message_id: String,
    pub operation_id: String,
    pub sender_thread: Option<String>,
    pub created_at: String,
    pub held: bool,
    pub hold_reason: Option<String>,
    pub uncertain: bool,
    pub now: bool,
    pub excerpt: String,
    pub excerpt_truncated: bool,
    pub attachment_count: u64,
}
impl PendingMessage {
    fn sender(&self) -> String {
        self.sender_thread
            .as_ref()
            .map(|id| format!("会話 {}", id.chars().take(8).collect::<String>()))
            .unwrap_or_else(|| "人".into())
    }
    fn label(&self) -> String {
        format!(
            "{} · {}{}{}",
            self.sender(),
            if self.uncertain {
                "反映状況不明"
            } else if self.held {
                "保留"
            } else {
                "反映待ち"
            },
            if self.now { " · すぐ渡す" } else { "" },
            self.hold_reason
                .as_deref()
                .map(|r| format!(
                    " · {}",
                    match r {
                        "terminal_before_path_admission" => "中断前の未反映",
                        "uncertain_path_admission" => "反映状況を確認できません",
                        "previous_instance_interrupted" => "前回の終了で中断",
                        "stopped" | "stopping" => "停止",
                        "recovery_wait" => "復旧待ち",
                        _ => "理由は詳細",
                    }
                ))
                .unwrap_or_default()
        )
    }
}
#[derive(Default)]
pub(super) struct QueuePopup {
    pub visible: bool,
    pub selected: usize,
    pub archives: Option<Vec<(String, String)>>,
    pub epoch: u64,
    pub archive_loading: bool,
    pub archive_error: Option<String>,
}
impl QueuePopup {
    pub(super) fn draw(&self, frame: &mut Frame<'_>, area: Rect, pending: &PendingMessages) {
        if !self.visible {
            return;
        }
        if let Some(archives) = &self.archives {
            let body = popup::panel(
                frame,
                area,
                "保存した下書き",
                &format!("{}件 · /restore draft ID", archives.len()),
                "Esc 戻る · ↑↓ 選択 · Enter 入力欄へ復元 · R 再取得",
            );
            let items = if archives.is_empty() {
                vec![popup::Item::new(if self.archive_loading {
                    "読込み中です".into()
                } else if let Some(error) = &self.archive_error {
                    format!(
                        "取得できません: {} · Rで再取得",
                        crate::presentation::sanitize_plain(error, 0).0
                    )
                } else {
                    "復元できる下書きはありません".into()
                })]
            } else {
                archives
                    .iter()
                    .map(|(id, label)| {
                        popup::Item::new(format!(
                            "{}\n{}",
                            crate::presentation::sanitize_plain(label, 0).0,
                            crate::presentation::sanitize_plain(id, 0).0
                        ))
                    })
                    .collect()
            };
            popup::list(
                frame,
                body,
                items,
                self.selected.min(archives.len().saturating_sub(1)),
            );
            return;
        }
        let context = format!(
            "{}件 / 表示{}件 · 保存済みの入力",
            pending.total,
            pending.items.len()
        );
        let body = popup::panel(
            frame,
            area,
            "反映待ち",
            &context,
            "Esc 戻る · ↑↓ 選択 · Enter 状態 · O 原文\nD 選択を取消 · A 表示中のIDを取消",
        );
        let items = if pending.items.is_empty() {
            vec![popup::Item::new("反映待ちはありません")]
        } else {
            pending
                .items
                .iter()
                .map(|item| {
                    popup::Item::new(vec![
                        Line::from(crate::presentation::sanitize_plain(&item.label(), 0).0)
                            .style(Style::default().fg(theme::EMPHASIS)),
                        Line::from(
                            crate::presentation::sanitize_plain(&item.excerpt, 0)
                                .0
                                .replace('\n', " "),
                        ),
                    ])
                })
                .collect()
        };
        popup::list(
            frame,
            body,
            items,
            self.selected.min(pending.items.len().saturating_sub(1)),
        );
    }
}
pub(super) fn height(view: &ThreadView, available: u16) -> u16 {
    let accepting = view
        .pending
        .values()
        .filter(|p| p.send.is_some() && p.status == ReceiptStatus::Accepting)
        .count();
    let count = view.pending_messages.items.len() + accepting;
    if count == 0 {
        return 0;
    }
    (u16::try_from(count.min(3)).unwrap_or(3) * 2 + 1).min(available.saturating_sub(3))
}
// Drawing must never scan or allocate the complete accepting draft: it may be
// several MiB while only one preview line is visible. Find a bounded UTF-8
// prefix before sanitization or newline replacement.
fn accepting_excerpt(text: &str) -> String {
    let end = text
        .char_indices()
        .nth(512)
        .map_or(text.len(), |(index, _)| index);
    let mut preview = crate::presentation::sanitize_plain(&text[..end], 0)
        .0
        .replace('\n', " ");
    if end < text.len() {
        preview.push('…');
    }
    preview
}
pub(super) fn draw_compact(frame: &mut Frame<'_>, area: Rect, view: &ThreadView) {
    if area.height == 0 {
        return;
    }
    let mut lines = vec![];
    for item in view.pending_messages.items.iter().take(3) {
        lines.push(Line::from(
            crate::presentation::sanitize_plain(&item.label(), 0).0,
        ));
        lines.push(Line::from(
            crate::presentation::sanitize_plain(&item.excerpt, 0)
                .0
                .replace('\n', " "),
        ));
    }
    for pending in view
        .pending
        .values()
        .filter(|p| p.send.is_some() && p.status == ReceiptStatus::Accepting)
        .take(3)
    {
        lines.push(Line::from(if pending.source_node_id.is_some() {
            "保存済み・取得中"
        } else {
            "送信受付中"
        }));
        lines.push(Line::from(accepting_excerpt(&pending.snapshot.text)));
    }
    lines.truncate(usize::from(area.height.saturating_sub(1)));
    lines.push(Line::from(format!(
        "/queue · 保存済み{}件",
        view.pending_messages.total
    )));
    frame.render_widget(
        Paragraph::new(lines).style(
            Style::default()
                .fg(theme::CODE_TEXT)
                .bg(theme::SELECTION_BACKGROUND),
        ),
        area,
    );
}

impl UiState {
    pub(super) fn bind_saved_history(
        &mut self,
        thread: &str,
        generation: u64,
        pending: &PendingMessages,
    ) {
        if self
            .views
            .values()
            .any(|v| v.thread_id.as_deref() == Some(thread) && v.generation == generation)
        {
            return;
        }
        if let Some(view) = self.views.values_mut().find(|view| {
            view.thread_id.is_none()
                && view.generation == generation
                && view.pending.values().any(|local| {
                    pending.submitted.iter().any(|item| {
                        item.source_draft_id == local.snapshot.draft_id
                            && item.revision == local.snapshot.revision
                            && local
                                .operation_id
                                .as_ref()
                                .is_none_or(|operation| operation == &item.operation_id)
                    }) || local.operation_id.as_ref().is_some_and(|operation| {
                        pending
                            .items
                            .iter()
                            .any(|item| &item.operation_id == operation)
                    })
                })
        }) {
            view.thread_id = Some(thread.into());
        }
    }
    pub(super) fn recovery_target(&self) -> DraftRecoveryTarget {
        let view = self.view();
        DraftRecoveryTarget {
            draft_id: view.draft.draft_id.clone(),
            revision: view.draft.revision,
            generation: view.generation,
            thread_id: view.thread_id.clone(),
        }
    }
    pub(super) fn queue_key(&mut self, key: KeyEvent) -> Vec<UiAction> {
        let count = self
            .queue_popup
            .archives
            .as_ref()
            .map_or(self.view().pending_messages.items.len(), Vec::len);
        match key.code {
            KeyCode::Esc => {
                self.queue_popup.visible = false;
                self.queue_popup.epoch = self.queue_popup.epoch.wrapping_add(1);
                vec![]
            }
            KeyCode::Up => {
                self.queue_popup.selected = self.queue_popup.selected.saturating_sub(1);
                vec![]
            }
            KeyCode::Down => {
                self.queue_popup.selected =
                    (self.queue_popup.selected + 1).min(count.saturating_sub(1));
                vec![]
            }
            KeyCode::Char('r') => {
                if self.queue_popup.archives.is_some() {
                    self.queue_popup.epoch = self.queue_popup.epoch.wrapping_add(1);
                    self.queue_popup.archive_loading = true;
                    self.queue_popup.archive_error = None;
                    return vec![UiAction::ListRestore {
                        thread_id: self.view().thread_id.clone(),
                        generation: self.view().generation,
                        request_id: self.queue_popup.epoch,
                    }];
                }
                self.view()
                    .thread_id
                    .clone()
                    .map(|thread_id| {
                        vec![UiAction::LoadHistory {
                            thread_id,
                            generation: self.view().generation,
                            before_cursor: None,
                        }]
                    })
                    .unwrap_or_default()
            }
            KeyCode::Enter => {
                if let Some(archives) = &self.queue_popup.archives {
                    if let Some((id, _)) = archives.get(self.queue_popup.selected) {
                        let id = id.clone();
                        self.queue_popup.visible = false;
                        return vec![UiAction::RestoreArchive {
                            archive_id: id,
                            target: self.recovery_target(),
                        }];
                    }
                } else if let Some(item) = self
                    .view()
                    .pending_messages
                    .items
                    .get(self.queue_popup.selected)
                {
                    let id = item.message_id.clone();
                    self.queue_popup.visible = false;
                    self.management.mode = management::ManagementMode::General;
                    self.management.open_text(ManagementView {
                        title: "依頼の状態".into(),
                        text: "取得中".into(),
                        scroll: 0,
                    });
                    return vec![UiAction::Manage(Command::MessageGet(
                        crate::protocol::MessageTarget { message_id: id },
                    ))];
                }
                vec![]
            }
            KeyCode::Char('o') if self.queue_popup.archives.is_none() => {
                if let Some(item) = self
                    .view()
                    .pending_messages
                    .items
                    .get(self.queue_popup.selected)
                {
                    let id = item.message_id.clone();
                    self.queue_popup.visible = false;
                    let mut details = details::Details::new(self.view(), false);
                    let action = details.open_source(id, &mut self.next_detail_request);
                    self.details = Some(details);
                    return vec![action];
                }
                vec![]
            }
            KeyCode::Char(mode @ ('d' | 'a')) if self.queue_popup.archives.is_none() => {
                let items = self
                    .view()
                    .pending_messages
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| mode == 'a' || *i == self.queue_popup.selected)
                    .map(|(_, item)| item.clone())
                    .collect::<Vec<_>>();
                if items.is_empty() {
                    return vec![];
                }
                let mut counts = BTreeMap::<String, usize>::new();
                for item in &items {
                    *counts
                        .entry(
                            item.sender_thread
                                .as_ref()
                                .map(|id| format!("会話 {id}"))
                                .unwrap_or_else(|| "人".into()),
                        )
                        .or_default() += 1;
                }
                let detail = format!(
                    "表示された{}件の入力を取り消しますか？\n{}\n新しく届いた入力は対象に含めません。取り消せた入力は /restore list に保存します。",
                    items.len(),
                    counts
                        .into_iter()
                        .map(|(who, n)| format!(
                            "{}: {n}件",
                            crate::presentation::sanitize_plain(&who, 0).0
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                let action = UiAction::CancelMessages {
                    message_ids: items.into_iter().map(|i| i.message_id).collect(),
                    target: self.recovery_target(),
                };
                self.confirmation = Some(Confirmation::MessageQueue {
                    action: Box::new(action),
                    detail,
                });
                vec![]
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }
    fn ui() -> UiState {
        UiState::new(&Config::default(), std::env::temp_dir())
    }
    fn queued(id: &str) -> PendingMessage {
        PendingMessage {
            message_id: id.into(),
            operation_id: format!("op:{id}"),
            sender_thread: None,
            created_at: "2026-10-30T00:00:00Z".into(),
            held: false,
            hold_reason: None,
            uncertain: false,
            now: false,
            excerpt: format!("input {id}"),
            excerpt_truncated: false,
            attachment_count: 0,
        }
    }
    fn page(
        ui: &mut UiState,
        revision: u64,
        pending: PendingMessages,
        nodes: Vec<UiNode>,
    ) -> UiUpdate {
        UiUpdate::History {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            nodes,
            pending,
            read_revision: revision,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
        }
    }
    fn selected(id: &str) -> UiNode {
        UiNode {
            node_id: id.into(),
            role: "user".into(),
            text: "selected input".into(),
            ..Default::default()
        }
    }
    #[test]
    fn late_paired_history_cannot_resurrect_cancelled_queue_or_replace_newer_history() {
        let mut ui = ui();
        ui.select_thread("thread".into());
        let old = page(
            &mut ui,
            10,
            PendingMessages {
                items: vec![queued("q")],
                total: 1,
                ..Default::default()
            },
            vec![selected("old")],
        );
        let fresh = page(
            &mut ui,
            11,
            PendingMessages::default(),
            vec![selected("fresh")],
        );
        ui.apply(fresh);
        ui.apply(old);
        assert_eq!(ui.view().pending_messages.total, 0);
        assert_eq!(ui.view().nodes()[0].node_id, "fresh");
        let view = ui.view();
        let old_window = UiUpdate::HistoryWindow {
            thread_id: "thread".into(),
            generation: view.generation,
            view_revision: view.view_revision,
            nodes: vec![selected("stale-window")],
            pending: PendingMessages {
                items: vec![queued("q")],
                total: 1,
                ..Default::default()
            },
            read_revision: 10,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
            anchor: ViewAnchor::default(),
        };
        ui.apply(old_window);
        assert_eq!(ui.view().pending_messages.total, 0);
        assert_eq!(ui.view().nodes()[0].node_id, "fresh");
    }
    #[test]
    fn committed_input_transfers_from_local_accepting_to_db_projection_atomically() {
        let mut ui = ui();
        ui.insert("hello", false);
        let send = ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
        let UiAction::Send {
            draft_id,
            revision,
            generation,
            snapshot: Some(snapshot),
            ..
        } = &send[0]
        else {
            panic!("exact source snapshot required");
        };
        assert_eq!(snapshot.text, "hello");
        let history = UiUpdate::History {
            thread_id: "thread".into(),
            generation: *generation,
            nodes: vec![],
            pending: PendingMessages {
                items: vec![queued("saved")],
                total: 1,
                ..Default::default()
            },
            read_revision: 1,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 0,
            total_nodes: 0,
        };
        ui.apply(UiUpdate::SavedMessage {
            draft_id: draft_id.clone(),
            revision: *revision,
            generation: *generation,
            thread_id: "thread".into(),
            status: ReceiptStatus::Accepted,
            history: Box::new(history),
        });
        assert!(ui.view().pending.is_empty());
        assert!(ui.view().draft.text.is_empty());
        assert_eq!(ui.view().pending_messages.items[0].message_id, "saved");
        assert!(ui.view().nodes().is_empty());
    }
    #[test]
    fn paired_db_commit_before_ack_reconciles_exact_operation_and_preserves_newer_edits() {
        for admitted in [false, true] {
            let mut ui = ui();
            ui.insert("original", false);
            let actions = ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
            let UiAction::Send {
                draft_id,
                revision,
                generation,
                ..
            } = &actions[0]
            else {
                panic!("send expected");
            };
            ui.apply(UiUpdate::SendOperation {
                draft_id: draft_id.clone(),
                revision: *revision,
                operation_id: "operation".into(),
            });
            ui.insert(" newer", false);
            let mut pending = PendingMessages::default();
            let nodes = if admitted {
                pending.submitted.push(SubmittedMessage {
                    operation_id: "operation".into(),
                    source_draft_id: draft_id.clone(),
                    revision: *revision,
                    message_id: "saved".into(),
                });
                vec![selected("saved")]
            } else {
                let mut item = queued("saved");
                item.operation_id = "operation".into();
                pending.items.push(item);
                pending.total = 1;
                vec![]
            };
            let projection = UiUpdate::History {
                thread_id: "thread".into(),
                generation: *generation,
                nodes,
                pending,
                read_revision: 1,
                before_cursor: None,
                append: false,
                first_ordinal: 0,
                selection_revision: 0,
                total_nodes: u64::from(admitted),
            };
            ui.apply(projection);
            assert!(
                ui.view().pending.is_empty(),
                "DB projection owns the saved input before its ACK"
            );
            assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
            assert_eq!(ui.view().draft.text, "original newer");
            assert_eq!(ui.view().pending_messages.total, u64::from(!admitted));
        }
    }
    #[test]
    fn db_pair_before_operation_update_uses_frozen_source_identity_without_duplicates() {
        let mut ui = ui();
        ui.insert("original", false);
        let actions = ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
        let UiAction::Send {
            draft_id,
            revision,
            generation,
            ..
        } = &actions[0]
        else {
            panic!("send expected");
        };
        let mut item = queued("saved");
        item.operation_id = "operation".into();
        let pending = PendingMessages {
            items: vec![item],
            total: 1,
            submitted: vec![SubmittedMessage {
                operation_id: "operation".into(),
                source_draft_id: draft_id.clone(),
                revision: *revision,
                message_id: "saved".into(),
            }],
        };
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation: *generation,
            nodes: vec![],
            pending,
            read_revision: 1,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 0,
            total_nodes: 0,
        });
        assert!(ui.view().pending.is_empty());
        assert_eq!(ui.view().pending_messages.total, 1);
        assert!(ui.view().draft.text.is_empty());
        ui.apply(UiUpdate::SendOperation {
            draft_id: draft_id.clone(),
            revision: *revision,
            operation_id: "operation".into(),
        });
        assert!(ui.view().pending.is_empty());
        assert_eq!(ui.view().pending_messages.items.len(), 1);
    }
    #[test]
    fn saved_ack_after_db_reconciliation_still_applies_newer_queue_without_clearing_edits() {
        let mut ui = ui();
        ui.insert("original", false);
        let actions = ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
        let UiAction::Send {
            draft_id,
            revision,
            generation,
            ..
        } = &actions[0]
        else {
            panic!("send expected");
        };
        let binding = SubmittedMessage {
            operation_id: "operation".into(),
            source_draft_id: draft_id.clone(),
            revision: *revision,
            message_id: "saved".into(),
        };
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation: *generation,
            nodes: vec![selected("saved")],
            pending: PendingMessages {
                submitted: vec![binding.clone()],
                ..Default::default()
            },
            read_revision: 1,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
        });
        assert!(ui.view().pending.is_empty());
        assert!(ui.view().draft.text.is_empty());
        ui.insert("newer draft", false);
        let history = UiUpdate::History {
            thread_id: "thread".into(),
            generation: *generation,
            nodes: vec![selected("saved")],
            pending: PendingMessages {
                items: vec![queued("newly-arrived")],
                total: 1,
                submitted: vec![binding],
            },
            read_revision: 2,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
        };
        ui.apply(UiUpdate::SavedMessage {
            draft_id: draft_id.clone(),
            revision: *revision,
            generation: *generation,
            thread_id: "thread".into(),
            status: ReceiptStatus::Accepted,
            history: Box::new(history),
        });
        assert_eq!(ui.view().draft.text, "newer draft");
        assert_eq!(ui.view().delivery_read_revision, 2);
        assert_eq!(
            ui.view().pending_messages.items[0].message_id,
            "newly-arrived"
        );
    }
    #[test]
    fn saved_ack_rejects_mismatched_embedded_thread_or_generation_before_draft_mutation() {
        for window in [false, true] {
            for wrong_thread in [false, true] {
                let mut ui = ui();
                ui.insert("original", false);
                let actions = ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
                let UiAction::Send {
                    draft_id,
                    revision,
                    generation,
                    ..
                } = &actions[0]
                else {
                    panic!("send expected");
                };
                let thread = if wrong_thread { "different" } else { "thread" };
                let embedded_generation = if wrong_thread {
                    *generation
                } else {
                    generation + 1
                };
                let history = if window {
                    UiUpdate::HistoryWindow {
                        thread_id: thread.into(),
                        generation: embedded_generation,
                        view_revision: ui.view().view_revision,
                        nodes: vec![],
                        pending: PendingMessages::default(),
                        read_revision: 2,
                        first_ordinal: 0,
                        selection_revision: 1,
                        total_nodes: 0,
                        anchor: ViewAnchor::default(),
                    }
                } else {
                    UiUpdate::History {
                        thread_id: thread.into(),
                        generation: embedded_generation,
                        nodes: vec![],
                        pending: PendingMessages::default(),
                        read_revision: 2,
                        before_cursor: None,
                        append: false,
                        first_ordinal: 0,
                        selection_revision: 1,
                        total_nodes: 0,
                    }
                };
                ui.apply(UiUpdate::SavedMessage {
                    draft_id: draft_id.clone(),
                    revision: *revision,
                    generation: *generation,
                    thread_id: "thread".into(),
                    status: ReceiptStatus::Accepted,
                    history: Box::new(history),
                });
                assert_eq!(ui.view().draft.text, "original");
                assert!(ui.view().thread_id.is_none());
                assert!(ui.view().pending.contains_key(revision));
            }
        }
    }
    #[test]
    fn old_generation_saved_ack_cannot_clear_a_newer_view_or_draft() {
        let mut ui = ui();
        ui.insert("original", false);
        let actions = ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
        let UiAction::Send {
            draft_id,
            revision,
            generation,
            ..
        } = &actions[0]
        else {
            panic!("send expected");
        };
        let history = UiUpdate::History {
            thread_id: "thread".into(),
            generation: *generation,
            nodes: vec![],
            pending: PendingMessages::default(),
            read_revision: 1,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 0,
            total_nodes: 0,
        };
        ui.active_view_mut().generation += 1;
        ui.apply(UiUpdate::SavedMessage {
            draft_id: draft_id.clone(),
            revision: *revision,
            generation: *generation,
            thread_id: "thread".into(),
            status: ReceiptStatus::Accepted,
            history: Box::new(history),
        });
        assert_eq!(ui.view().draft.text, "original");
        assert!(ui.view().thread_id.is_none());
        assert!(ui.view().pending.contains_key(revision));
    }
    #[test]
    fn closed_archive_popup_is_not_reopened_by_a_late_reply() {
        let mut ui = ui();
        let actions = ui.slash("/restore list");
        let UiAction::ListRestore {
            thread_id,
            generation,
            request_id,
        } = &actions[0]
        else {
            panic!("archive query expected");
        };
        ui.event(key(KeyCode::Esc, KeyModifiers::NONE));
        ui.apply(UiUpdate::RestoreArchives {
            thread_id: thread_id.clone(),
            generation: *generation,
            request_id: *request_id,
            entries: vec![("archive".into(), "entry".into())],
            error: None,
        });
        assert!(!ui.queue_popup.visible);
    }
    #[test]
    fn now_keys_respect_configured_normal_send_and_chord_focus() {
        for normal in ["Enter", "Ctrl+Enter"] {
            let mut config = Config::default();
            config.ui.send_key = normal.into();
            let mut ui = UiState::new(&config, std::env::temp_dir());
            ui.insert("normal", false);
            let actions = ui.event(key(KeyCode::Enter, KeyModifiers::CONTROL));
            assert!(matches!(&actions[0],UiAction::Send {now,..} if *now==(normal!="Ctrl+Enter")));
            let mut ui = UiState::new(&config, std::env::temp_dir());
            ui.insert("urgent", false);
            assert!(
                ui.event(key(KeyCode::Char('x'), KeyModifiers::CONTROL))
                    .is_empty()
            );
            assert!(matches!(
                &ui.event(key(KeyCode::Char('s'), KeyModifiers::CONTROL))[0],
                UiAction::Send { now: true, .. }
            ));
        }
        let mut ui = ui();
        ui.insert("keep", false);
        ui.event(key(KeyCode::Char('x'), KeyModifiers::CONTROL));
        ui.event(key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(ui.view().draft.text, "keepa");
        ui.event(key(KeyCode::Char('x'), KeyModifiers::CONTROL));
        ui.event(key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert!(
            ui.event(key(KeyCode::Char('s'), KeyModifiers::CONTROL))
                .is_empty()
        );
    }
    #[test]
    fn queue_confirmation_uses_only_the_displayed_ids_and_sanitizes_labels() {
        let mut ui = ui();
        ui.select_thread("thread".into());
        let mut foreign = queued("second");
        foreign.sender_thread = Some("other-thread".into());
        foreign.hold_reason = Some("terminal_before_path_admission".into());
        foreign.excerpt = "safe\x1b[2Jtext".into();
        let update = page(
            &mut ui,
            1,
            PendingMessages {
                items: vec![queued("first"), foreign],
                total: 2,
                ..Default::default()
            },
            vec![],
        );
        ui.apply(update);
        ui.slash("/queue");
        ui.event(key(KeyCode::Char('a'), KeyModifiers::NONE));
        let Some(Confirmation::MessageQueue { detail, .. }) = &ui.confirmation else {
            panic!("confirmation required");
        };
        assert!(detail.contains("other-thread: 1件"));
        let update = page(
            &mut ui,
            2,
            PendingMessages {
                items: vec![queued("first"), queued("second"), queued("new")],
                total: 3,
                ..Default::default()
            },
            vec![],
        );
        ui.apply(update);
        let actions = ui.event(key(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(
            matches!(&actions[0],UiAction::CancelMessages {message_ids,..} if message_ids==&["first","second"])
        );
        assert!(!ui.queue_popup.visible);
    }
    #[test]
    fn late_restore_preserves_new_edits_and_original_attachment_fold_layout() {
        let mut ui = ui();
        let target = ui.recovery_target();
        let snapshot = DraftSnapshot {
            text: "restored".into(),
            cursor: 8,
            folds: vec![PasteFold {
                id: 7,
                start: 0,
                end: 8,
                expanded: false,
            }],
            ..Default::default()
        };
        ui.apply(UiUpdate::DraftRecovery {
            target: target.clone(),
            draft: Some(Box::new(snapshot.clone())),
            message: "restored".into(),
            notification_id: Some("operation".into()),
        });
        assert_eq!(ui.view().draft.folds, snapshot.folds);
        let target = ui.recovery_target();
        ui.insert(" newer", false);
        ui.apply(UiUpdate::DraftRecovery {
            target,
            draft: Some(Box::new(DraftSnapshot {
                text: "must-not-overwrite".into(),
                ..Default::default()
            })),
            message: "archived".into(),
            notification_id: Some("second-operation".into()),
        });
        assert_eq!(ui.view().draft.text, "restored newer");
        ui.apply(UiUpdate::DraftRecovery {
            target: ui.recovery_target(),
            draft: None,
            message: "duplicate must not notify".into(),
            notification_id: Some("operation".into()),
        });
        assert_eq!(ui.view().status, "archived");
    }
    #[test]
    fn escape_dismisses_queue_before_interrupting_exact_turn_and_ctrl_c_stops_run() {
        let mut ui = ui();
        ui.select_thread("thread".into());
        ui.active_view_mut().activity.snapshot = Some(activity::ActivitySnapshot {
            turn_id: Some("turn".into()),
            turn_in_progress: true,
            ..Default::default()
        });
        ui.slash("/queue");
        assert!(ui.event(key(KeyCode::Esc, KeyModifiers::NONE)).is_empty());
        assert!(
            matches!(&ui.event(key(KeyCode::Esc,KeyModifiers::NONE))[0],UiAction::InterruptTurn {thread_id,turn_id} if thread_id=="thread"&&turn_id=="turn")
        );
        assert!(
            matches!(&ui.event(key(KeyCode::Char('c'),KeyModifiers::CONTROL))[0],UiAction::Cancel {thread_id} if thread_id=="thread")
        );
    }
    #[test]
    fn compact_queue_and_popup_bound_terminal_control_text() {
        let mut ui = ui();
        let mut pending = queued("id");
        pending.excerpt = "visible\x1b[2J text".into();
        pending.held = true;
        pending.hold_reason = Some("terminal_before_path_admission".into());
        ui.active_view_mut().pending_messages = PendingMessages {
            items: vec![pending],
            total: 1,
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| super::draw_compact(frame, Rect::new(0, 0, 100, 5), ui.view()))
            .unwrap();
        let mut shown = String::new();
        let buffer = terminal.backend().buffer();
        for y in 0..30 {
            let mut x = 0;
            while x < 100 {
                let symbol = buffer[(x, y)].symbol();
                shown.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            shown.push('\n');
        }
        assert!(shown.contains("中断前の未反映"));
        assert!(!shown.contains("terminal_before_path_admission"));
        assert!(!shown.contains('\x1b'));
    }
    #[test]
    fn accepting_large_draft_renders_only_a_sanitized_bounded_prefix() {
        let mut ui = ui();
        ui.insert("source", false);
        ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
        let body = format!(
            "VISIBLE\x1b[2J {} TAIL_MUST_NOT_APPEAR",
            "x".repeat(4 * 1024 * 1024)
        );
        let preview = accepting_excerpt(&body);
        assert!(preview.starts_with("VISIBLE"));
        assert!(preview.ends_with('…'));
        assert!(preview.len() < 4096);
        assert!(!preview.contains("TAIL_MUST_NOT_APPEAR"));
        assert!(!preview.contains('\x1b'));
        ui.active_view_mut()
            .pending
            .values_mut()
            .next()
            .unwrap()
            .snapshot
            .text = body;
        let mut terminal = Terminal::new(TestBackend::new(100, 8)).unwrap();
        terminal
            .draw(|frame| super::draw_compact(frame, Rect::new(0, 0, 100, 5), ui.view()))
            .unwrap();
        let shown = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(shown.contains("VISIBLE"));
        assert!(!shown.contains("TAIL_MUST_NOT_APPEAR"));
        assert!(!shown.contains('\x1b'));
    }
}
