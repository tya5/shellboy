//! Current saved input, separate from immutable selected conversation history.
use super::*;
use crate::config::UiLanguage;
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
    fn sender(&self, language: UiLanguage) -> String {
        match (&self.sender_thread, language) {
            (Some(id), UiLanguage::Ja) => {
                format!("会話 {}", id.chars().take(8).collect::<String>())
            }
            (Some(id), UiLanguage::En) => {
                format!("Conversation {}", id.chars().take(8).collect::<String>())
            }
            (None, _) => i18n::local!(language, "人", "Person").into(),
        }
    }
    fn label(&self, language: UiLanguage) -> String {
        let sender = self.sender(language);
        let mut label = match (language, self.uncertain, self.held, self.now) {
            (UiLanguage::Ja, true, _, false) => format!("{sender} | 反映状況不明"),
            (UiLanguage::Ja, true, _, true) => format!("{sender} | 反映状況不明 | すぐ渡す"),
            (UiLanguage::Ja, false, true, false) => format!("{sender} | 保留"),
            (UiLanguage::Ja, false, true, true) => format!("{sender} | 保留 | すぐ渡す"),
            (UiLanguage::Ja, false, false, false) => format!("{sender} | 反映待ち"),
            (UiLanguage::Ja, false, false, true) => format!("{sender} | 反映待ち | すぐ渡す"),
            (UiLanguage::En, true, _, false) => format!("{sender} | Delivery unknown"),
            (UiLanguage::En, true, _, true) => format!("{sender} | Delivery unknown | Deliver now"),
            (UiLanguage::En, false, true, false) => format!("{sender} | Held"),
            (UiLanguage::En, false, true, true) => format!("{sender} | Held | Deliver now"),
            (UiLanguage::En, false, false, false) => format!("{sender} | Pending"),
            (UiLanguage::En, false, false, true) => format!("{sender} | Pending | Deliver now"),
        };
        if let Some(reason) = &self.hold_reason {
            // A reason is an independent fact, not a grammatical message fragment.
            label.push_str(" | ");
            label.push_str(match reason.as_str() {
                "terminal_before_path_admission" => i18n::local!(
                    language,
                    "中断前の未反映",
                    "Not delivered before interruption"
                ),
                "uncertain_path_admission" => i18n::local!(
                    language,
                    "反映状況を確認できません",
                    "Delivery could not be verified"
                ),
                "previous_instance_interrupted" => i18n::local!(
                    language,
                    "前回の終了で中断",
                    "Interrupted by previous shutdown"
                ),
                "stopped" | "stopping" => i18n::local!(language, "停止", "Stopped"),
                "recovery_wait" => i18n::local!(language, "復旧待ち", "Waiting for recovery"),
                _ => reason,
            });
        }
        label
    }
}
fn archive_count(language: UiLanguage, count: usize) -> String {
    match language {
        UiLanguage::Ja => format!("{count}件 | /restore draft ID"),
        UiLanguage::En if count == 1 => "1 draft | /restore draft ID".into(),
        UiLanguage::En => format!("{count} drafts | /restore draft ID"),
    }
}
fn queue_context(language: UiLanguage, total: u64, shown: usize) -> String {
    match language {
        UiLanguage::Ja => format!("{total}件 / 表示{shown}件 | 保存済みの入力"),
        UiLanguage::En => format!("Saved input: {total} total / {shown} shown"),
    }
}
fn compact_count(language: UiLanguage, count: u64) -> String {
    match language {
        UiLanguage::Ja => format!("/queue | 保存済み{count}件"),
        UiLanguage::En if count == 1 => "/queue | 1 saved input".into(),
        UiLanguage::En => format!("/queue | {count} saved inputs"),
    }
}
fn archive_error(language: UiLanguage, error: &str) -> String {
    let error = crate::presentation::sanitize_plain(error, 0).0;
    match language {
        UiLanguage::Ja => format!("取得できません: {error} | Rで再取得"),
        UiLanguage::En => format!("Could not load: {error} | R Retry"),
    }
}
fn sender_count(language: UiLanguage, sender: Option<&str>, count: usize) -> String {
    match (language, sender, count == 1) {
        (UiLanguage::Ja, Some(id), _) => format!("会話 {id}: {count}件"),
        (UiLanguage::Ja, None, _) => format!("人: {count}件"),
        (UiLanguage::En, Some(id), true) => format!("Conversation {id}: 1 input"),
        (UiLanguage::En, Some(id), false) => format!("Conversation {id}: {count} inputs"),
        (UiLanguage::En, None, true) => "Person: 1 input".into(),
        (UiLanguage::En, None, false) => format!("Person: {count} inputs"),
    }
}
fn cancel_detail(language: UiLanguage, count: usize, senders: &str) -> String {
    match language {
        UiLanguage::Ja => format!(
            "表示された{count}件の入力を取り消しますか？\n{senders}\n新しく届いた入力は対象に含めません。取り消せた入力は /restore list に保存します。"
        ),
        UiLanguage::En if count == 1 => format!(
            "Cancel 1 displayed input?\n{senders}\nNew arrivals are excluded. Cancelled input is saved in /restore list."
        ),
        UiLanguage::En => format!(
            "Cancel {count} displayed inputs?\n{senders}\nNew arrivals are excluded. Cancelled inputs are saved in /restore list."
        ),
    }
}
#[derive(Default)]
pub(super) struct QueuePopup {
    paint: std::cell::Cell<Option<popup::PaintBounds>>,
    pub visible: bool,
    pub selected: usize,
    pub archives: Option<Vec<(String, String)>>,
    pub epoch: u64,
    pub archive_loading: bool,
    pub archive_error: Option<String>,
}
impl QueuePopup {
    pub(super) fn paint_bounds(&self) -> Option<popup::PaintBounds> {
        self.paint.get()
    }
    pub(super) fn wheel(&mut self, delta: isize, pending_count: usize) -> bool {
        let count = self.archives.as_ref().map_or(pending_count, Vec::len);
        let old = self.selected;
        self.selected = popup::move_selection(old, delta, count);
        old != self.selected
    }

    pub(super) fn draw(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        pending: &PendingMessages,
        language: UiLanguage,
    ) {
        if !self.visible {
            return;
        }
        if let Some(archives) = &self.archives {
            let body = popup::panel_recorded(
                frame,
                area,
                i18n::local!(language, "保存した下書き", "Saved drafts"),
                &archive_count(language, archives.len()),
                i18n::local!(
                    language,
                    "Esc 戻る | ^v 選択 | Enter 入力欄へ復元 | R 再取得",
                    "Esc Back | ^v Select | Enter Restore draft | R Retry"
                ),
                &self.paint,
            );
            let items = if archives.is_empty() {
                vec![popup::Item::new(if self.archive_loading {
                    i18n::local!(language, "読込み中です", "Loading").into()
                } else if let Some(error) = &self.archive_error {
                    archive_error(language, error)
                } else {
                    i18n::local!(
                        language,
                        "復元できる下書きはありません",
                        "No drafts to restore"
                    )
                    .into()
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
        let context = queue_context(language, pending.total, pending.items.len());
        let body = popup::panel_recorded(
            frame,
            area,
            i18n::local!(language, "反映待ち", "Pending input"),
            &context,
            i18n::local!(
                language,
                "Esc 戻る | ^v 選択 | Enter 状態 | O 原文\nD 選択を取消 | A 表示中のIDを取消",
                "Esc Back | ^v Select | Enter Status | O Source\nD Cancel selected | A Cancel displayed IDs"
            ),
            &self.paint,
        );
        let items = if pending.items.is_empty() {
            vec![popup::Item::new(i18n::local!(
                language,
                "反映待ちはありません",
                "No pending input"
            ))]
        } else {
            pending
                .items
                .iter()
                .map(|item| {
                    let mut lines = presentation::hanging_wrap(
                        vec![
                            Line::from(
                                crate::presentation::sanitize_plain(&item.label(language), 0).0,
                            )
                            .style(Style::default().fg(theme::EMPHASIS)),
                        ],
                        body.width.saturating_sub(3).max(1),
                    );
                    lines.push(Line::from(
                        crate::presentation::sanitize_plain(&item.excerpt, 0)
                            .0
                            .replace('\n', " "),
                    ));
                    popup::Item::new(lines)
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
        preview.push('.');
    }
    preview
}
// Compact rows keep their fixed height; /queue exposes the complete wrapped label.
fn compact_label(label: &str, width: usize) -> String {
    use crate::ui::text_width::UnicodeWidthStr;
    use unicode_segmentation::UnicodeSegmentation;
    if label.width() <= width {
        return label.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut used = 0;
    for glyph in label.graphemes(true) {
        let cells = glyph.width();
        if used + cells + 1 > width {
            break;
        }
        output.push_str(glyph);
        used += cells;
    }
    output.push('.');
    output
}
pub(super) fn draw_compact(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &ThreadView,
    language: UiLanguage,
) {
    if area.height == 0 {
        return;
    }
    let mut lines = vec![];
    for item in view.pending_messages.items.iter().take(3) {
        lines.push(Line::from(compact_label(
            &crate::presentation::sanitize_plain(&item.label(language), 0).0,
            usize::from(area.width),
        )));
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
            i18n::local!(language, "保存済み・取得中", "Saved; loading")
        } else {
            i18n::local!(language, "送信受付中", "Submitting")
        }));
        lines.push(Line::from(accepting_excerpt(&pending.snapshot.text)));
    }
    lines.truncate(usize::from(area.height.saturating_sub(1)));
    lines.push(Line::from(compact_count(
        language,
        view.pending_messages.total,
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
        let key = key_hints::readonly_key(key);
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
                        title: i18n::local!(
                            self.config.ui.language,
                            "依頼の状態",
                            "Request status"
                        )
                        .into(),
                        text: i18n::local!(self.config.ui.language, "取得中", "Loading").into(),
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
                let mut counts = BTreeMap::<Option<String>, usize>::new();
                for item in &items {
                    *counts.entry(item.sender_thread.clone()).or_default() += 1;
                }
                let language = self.config.ui.language;
                let senders = counts
                    .into_iter()
                    .map(|(who, count)| {
                        let who = who
                            .as_deref()
                            .map(|id| crate::presentation::sanitize_plain(id, 0).0);
                        sender_count(language, who.as_deref(), count)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let detail = cancel_detail(language, items.len(), &senders);
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
            view_revision: view.navigation.view_revision,
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
                        view_revision: ui.view().navigation.view_revision,
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
        ui.config.ui.language = UiLanguage::Ja;
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
            .draw(|frame| {
                super::draw_compact(frame, Rect::new(0, 0, 100, 5), ui.view(), UiLanguage::Ja)
            })
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
        assert!(preview.ends_with('.'));
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
            .draw(|frame| {
                super::draw_compact(frame, Rect::new(0, 0, 100, 5), ui.view(), UiLanguage::En)
            })
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

#[cfg(test)]
mod language_tests {
    use super::*;
    fn item(id: &str) -> PendingMessage {
        PendingMessage {
            message_id: id.into(),
            operation_id: format!("operation-{id}"),
            sender_thread: None,
            created_at: "raw-time".into(),
            held: true,
            hold_reason: Some("recovery_wait".into()),
            uncertain: false,
            now: true,
            excerpt: "source-preview".into(),
            excerpt_truncated: false,
            attachment_count: 0,
        }
    }
    fn pending(ui: &mut UiState) {
        ui.queue_popup.visible = true;
        ui.view_mut().pending_messages = PendingMessages {
            items: vec![item("message-id")],
            total: 1,
            ..Default::default()
        };
    }
    fn archives(ui: &mut UiState) {
        ui.queue_popup.visible = true;
        ui.queue_popup.archives = Some(vec![]);
    }
    #[test]
    fn queue_and_archive_screens_are_registered_in_both_languages() {
        i18n::verify_screen(
            |ui| ui.queue_popup.visible = true,
            |language| match language {
                UiLanguage::En => &[
                    "Pending input",
                    "No pending input",
                    "Enter Status",
                    "O Source",
                    "D Cancel selected",
                    "A Cancel displayed IDs",
                ],
                UiLanguage::Ja => &[
                    "反映待ち",
                    "反映待ちはありません",
                    "Enter 状態",
                    "O 原文",
                    "D 選択を取消",
                    "A 表示中のIDを取消",
                ],
            },
        );
        i18n::verify_screen(pending, |language| match language {
            UiLanguage::En => &[
                "Person | Held | Deliver now | Waiting for recovery",
                "1 total / 1 shown",
                "source-preview",
            ],
            UiLanguage::Ja => &[
                "人 | 保留 | すぐ渡す | 復旧待ち",
                "1件 / 表示1件",
                "source-preview",
            ],
        });
        i18n::verify_screen(
            |ui| {
                pending(ui);
                ui.queue_popup.visible = false;
            },
            |language| match language {
                UiLanguage::En => &[
                    "Person | Held | Deliver now | Waiting for recovery",
                    "source-preview",
                    "/queue | 1 saved input",
                ],
                UiLanguage::Ja => &[
                    "人 | 保留 | すぐ渡す | 復旧待ち",
                    "source-preview",
                    "/queue | 保存済み1件",
                ],
            },
        );
        i18n::verify_screen(archives, |language| match language {
            UiLanguage::En => &[
                "Saved drafts",
                "No drafts to restore",
                "0 drafts",
                "Enter Restore draft",
                "R Retry",
            ],
            UiLanguage::Ja => &[
                "保存した下書き",
                "復元できる下書きはありません",
                "0件",
                "Enter 入力欄へ復元",
                "R 再取得",
            ],
        });
        i18n::verify_screen(
            |ui| {
                archives(ui);
                ui.queue_popup.archive_loading = true;
            },
            |language| match language {
                UiLanguage::En => &["Saved drafts", "Loading", "R Retry"],
                UiLanguage::Ja => &["保存した下書き", "読込み中です", "R 再取得"],
            },
        );
        i18n::verify_screen(
            |ui| {
                archives(ui);
                ui.queue_popup.archive_error = Some("raw-load-error".into());
            },
            |language| match language {
                UiLanguage::En => &["Could not load: raw-load-error | R Retry"],
                UiLanguage::Ja => &["取得できません: raw-load-error | Rで再取得"],
            },
        );
        i18n::verify_screen(
            |ui| {
                archives(ui);
                ui.queue_popup.archives = Some(vec![("draft-id".into(), "source-title".into())]);
            },
            |language| match language {
                UiLanguage::En => &["1 draft | /restore draft ID", "source-title", "draft-id"],
                UiLanguage::Ja => &["1件 | /restore draft ID", "source-title", "draft-id"],
            },
        );
    }
    #[test]
    fn counts_and_cancel_messages_are_complete_for_zero_one_and_many() {
        for count in [0, 1, 2, 123] {
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let archive = archive_count(language, count);
                let compact = compact_count(language, count as u64);
                let sender = sender_count(language, Some("raw-id"), count);
                let cancel = cancel_detail(language, count, &sender);
                if language == UiLanguage::Ja {
                    assert_eq!(archive, format!("{count}件 | /restore draft ID"));
                    assert_eq!(compact, format!("/queue | 保存済み{count}件"));
                    assert_eq!(sender, format!("会話 raw-id: {count}件"));
                    assert!(
                        cancel.starts_with(&format!("表示された{count}件の入力を取り消しますか？"))
                    );
                    assert!(cancel.contains("新しく届いた入力は対象に含めません。"));
                } else if count == 1 {
                    assert_eq!(archive, "1 draft | /restore draft ID");
                    assert_eq!(compact, "/queue | 1 saved input");
                    assert_eq!(sender, "Conversation raw-id: 1 input");
                    assert!(cancel.starts_with("Cancel 1 displayed input?"));
                    assert!(cancel.ends_with("Cancelled input is saved in /restore list."));
                } else {
                    assert_eq!(archive, format!("{count} drafts | /restore draft ID"));
                    assert_eq!(compact, format!("/queue | {count} saved inputs"));
                    assert_eq!(sender, format!("Conversation raw-id: {count} inputs"));
                    assert!(cancel.starts_with(&format!("Cancel {count} displayed inputs?")));
                    assert!(cancel.ends_with("Cancelled inputs are saved in /restore list."));
                }
            }
        }
    }
    #[test]
    fn state_projection_preserves_unknown_reasons_and_source_facts() {
        let mut pending = item("raw-message-id");
        pending.sender_thread = Some("原文·👩‍💻-thread".into());
        pending.hold_reason = Some("raw-原文·👩‍💻-reason".into());
        pending.excerpt = "source-原文·👩‍💻".into();
        let original = pending.clone();
        for language in [UiLanguage::En, UiLanguage::Ja] {
            for uncertain in [false, true] {
                for held in [false, true] {
                    for now in [false, true] {
                        pending.uncertain = uncertain;
                        pending.held = held;
                        pending.now = now;
                        let before = pending.clone();
                        let label = pending.label(language);
                        assert!(label.contains("原文·👩‍💻"));
                        assert!(label.ends_with(" | raw-原文·👩‍💻-reason"));
                        if language == UiLanguage::En {
                            let state = if uncertain {
                                "Delivery unknown"
                            } else if held {
                                "Held"
                            } else {
                                "Pending"
                            };
                            assert!(label.contains(state));
                            assert_eq!(label.contains("Deliver now"), now);
                        } else {
                            let state = if uncertain {
                                "反映状況不明"
                            } else if held {
                                "保留"
                            } else {
                                "反映待ち"
                            };
                            assert!(label.contains(state));
                            assert_eq!(label.contains("すぐ渡す"), now);
                        }
                        assert_eq!(pending, before);
                    }
                }
            }
        }
        assert_eq!(pending.excerpt, original.excerpt);
        assert_eq!(pending.hold_reason, original.hold_reason);
        assert_eq!(pending.message_id, original.message_id);
    }
    fn long_pending(ui: &mut UiState) {
        pending(ui);
        let item = &mut ui.view_mut().pending_messages.items[0];
        item.sender_thread = Some("12345678-rest".into());
        item.uncertain = true;
        item.hold_reason = Some("terminal_before_path_admission".into());
    }
    #[test]
    fn real_sender_and_recovery_facts_remain_visible_at_screen_boundaries() {
        i18n::verify_screen(long_pending, |language| match language {
            UiLanguage::En => &[
                "Conversation 12345678",
                "Delivery unknown",
                "Deliver now",
                "Not",
                "delivered",
                "before",
                "interruption",
            ],
            UiLanguage::Ja => &[
                "会話 12345678",
                "反映状況不明",
                "すぐ渡す",
                "中断前の未反映",
            ],
        });
        use crate::ui::text_width::UnicodeWidthStr;
        for observed in [Some(1), Some(2), None] {
            crate::ui::text_width::set_terminal_width(observed);
            for width in [80, 120] {
                for language in [UiLanguage::En, UiLanguage::Ja] {
                    let mut config = Config::default();
                    config.ui.language = language;
                    let mut ui = UiState::new(&config, PathBuf::new());
                    long_pending(&mut ui);
                    let before = ui.view().pending_messages.clone();
                    let mut terminal =
                        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 30))
                            .unwrap();
                    terminal
                        .draw(|frame| super::super::draw(frame, &mut ui))
                        .unwrap();
                    let bounds = ui.queue_popup.paint_bounds().unwrap();
                    let buffer = terminal.backend().buffer();
                    for y in bounds.outer.y + 1..bounds.outer.bottom() - 1 {
                        assert_eq!(buffer[(bounds.outer.x, y)].symbol(), "|");
                        assert_eq!(buffer[(bounds.outer.right() - 1, y)].symbol(), "|");
                    }
                    let mut body = String::new();
                    for y in bounds.body.y..bounds.body.bottom() {
                        let mut x = bounds.body.x;
                        while x < bounds.body.right() {
                            let symbol = buffer[(x, y)].symbol();
                            body.push_str(symbol);
                            x += symbol.width().max(1) as u16;
                        }
                        body.push(' ');
                    }
                    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
                    assert!(
                        body.contains(&before.items[0].label(language)),
                        "{language:?} width={width}: {body}"
                    );
                    assert_eq!(ui.view().pending_messages, before);
                    let label = compact_label(&before.items[0].label(language), usize::from(width));
                    assert!(label.width() <= usize::from(width));
                    if before.items[0].label(language).width() > usize::from(width) {
                        assert!(label.ends_with('.'));
                    }
                }
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }
    #[test]
    fn compact_ellipsis_preserves_graphemes_cells_and_fixed_height() {
        use crate::ui::text_width::UnicodeWidthStr;
        use unicode_segmentation::UnicodeSegmentation;
        let source = "state 日本語·e\u{301}👩‍💻 trailing";
        for observed in [Some(1), Some(2), None] {
            crate::ui::text_width::set_terminal_width(observed);
            for width in 0..60 {
                let label = compact_label(source, width);
                assert!(label.width() <= width);
                if label != source && !label.is_empty() {
                    assert!(label.ends_with('.'));
                    let prefix = label.strip_suffix('.').unwrap();
                    let count = prefix.graphemes(true).count();
                    assert_eq!(
                        prefix,
                        source.graphemes(true).take(count).collect::<String>()
                    );
                }
            }
            let mut config = Config::default();
            config.ui.language = UiLanguage::En;
            let mut ui = UiState::new(&config, PathBuf::new());
            long_pending(&mut ui);
            ui.queue_popup.visible = false;
            assert_eq!(height(ui.view(), 30), 3);
        }
        crate::ui::text_width::clear_terminal_width();
    }
    #[test]
    fn locale_does_not_expand_the_captured_cancellation_scope() {
        for language in [UiLanguage::En, UiLanguage::Ja] {
            let mut config = Config::default();
            config.ui.language = language;
            let mut ui = UiState::new(&config, PathBuf::new());
            ui.select_thread("thread-id".into());
            pending(&mut ui);
            ui.view_mut().pending_messages.items.push(item("second-id"));
            ui.view_mut().pending_messages.total = 2;
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
            )));
            let Some(Confirmation::MessageQueue { detail, action }) = &ui.confirmation else {
                panic!("confirmation required");
            };
            assert!(detail.contains("/restore list"));
            assert!(
                matches!(action.as_ref(), UiAction::CancelMessages { message_ids, .. } if message_ids == &["message-id", "second-id"])
            );
            ui.view_mut()
                .pending_messages
                .items
                .push(item("new-arrival"));
            let actions = ui.event(Event::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
            )));
            assert!(actions.iter().any(|action| matches!(action, UiAction::CancelMessages { message_ids, .. } if message_ids == &["message-id", "second-id"])));
        }
    }
}
