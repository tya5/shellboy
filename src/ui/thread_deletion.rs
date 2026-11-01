//! Request-scoped deletion review on the existing confirmation overlay.
use super::i18n::local;
use super::*;
use crate::config::UiLanguage;
use serde_json::Value;
fn failure_text(error: &crate::protocol::ApiError, language: UiLanguage) -> String {
    let uncertain = error
        .details
        .as_ref()
        .and_then(|details| details.get("deletion_outcome_uncertain"))
        .and_then(Value::as_bool)
        == Some(true);
    match (language, uncertain) {
        (UiLanguage::En, true) => format!("Deletion outcome is unknown: {}", error.message),
        (UiLanguage::En, false) => format!("Could not delete: {}", error.message),
        (UiLanguage::Ja, true) => format!("削除結果は未確認です: {}", error.message),
        (UiLanguage::Ja, false) => format!("削除できません: {}", error.message),
    }
}
#[derive(Clone, Debug)]
enum Phase {
    Loading,
    Ready(Value),
    Stale(Option<Value>),
    Executing,
    Failed(crate::protocol::ApiError),
    InvalidPreview,
}
#[derive(Clone, Debug)]
pub(super) struct DeletionDialog {
    language: UiLanguage,
    request_id: u64,
    args: ThreadDeleteArgs,
    title: String,
    captured_at: Option<chrono::DateTime<chrono::Utc>>,
    phase: Phase,
}
impl DeletionDialog {
    pub(super) fn footer(&self) -> &'static str {
        if self.can_execute() {
            local!(
                self.language,
                "D / Space 子の会話も削除を切替 | N / Esc 戻る\nY 削除 | ^v/PgUp/PgDn 閲覧",
                "D / Space Toggle descendants | N / Esc Back\nY Delete | ^v/PgUp/PgDn Scroll"
            )
        } else {
            local!(
                self.language,
                "D / Space 子の会話も削除を切替 | N / Esc 戻る\nY 使用不可 | ^v/PgUp/PgDn 閲覧",
                "D / Space Toggle descendants | N / Esc Back\nY disabled | ^v/PgUp/PgDn Scroll"
            )
        }
    }
    fn matches(&self, request_id: u64, args: &ThreadDeleteArgs) -> bool {
        self.request_id == request_id
            && self.args.thread_id == args.thread_id
            && self.args.descendants == args.descendants
    }
    pub(super) fn invalidate(&mut self) {
        self.phase = match std::mem::replace(&mut self.phase, Phase::Stale(None)) {
            Phase::Ready(value) => Phase::Stale(Some(value)),
            Phase::Loading => Phase::Stale(None),
            other => other,
        };
    }
    fn can_execute(&self) -> bool {
        self.args.preview_marker.is_some()
            && matches!(&self.phase,Phase::Ready(value) if value.get("deletable_now").and_then(Value::as_bool)==Some(true))
    }
    pub(super) fn text(&self) -> String {
        let language = self.language;
        let title = &self.title;
        let thread = &self.args.thread_id;
        let mut text = match (language, self.args.descendants) {
            (UiLanguage::Ja, true) => {
                format!("会話の削除確認\n対象: {title}\n会話ID: {thread}\n子の会話も削除: する\n")
            }
            (UiLanguage::Ja, false) => {
                format!("会話の削除確認\n対象: {title}\n会話ID: {thread}\n子の会話も削除: しない\n")
            }
            (UiLanguage::En, true) => format!(
                "Confirm conversation deletion\nTarget: {title}\nConversation ID: {thread}\nDelete descendants: yes\n"
            ),
            (UiLanguage::En, false) => format!(
                "Confirm conversation deletion\nTarget: {title}\nConversation ID: {thread}\nDelete descendants: no\n"
            ),
        };
        if let Some(captured_at) = self.captured_at {
            let timestamp = captured_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            text.push_str(&match language {
                UiLanguage::Ja => format!("確認時刻: {timestamp}\n"),
                UiLanguage::En => format!("Reviewed at: {timestamp}\n"),
            });
        }
        match &self.phase {
            Phase::Loading => text.push_str(local!(language,
                "\n削除影響を確認中. Y はまだ使えません。",
                "\nChecking deletion impact. Y is not available yet.")),
            Phase::Executing => text.push_str(local!(language,
                "\n削除を実行中. 閉じても受理済みの処理は継続します。",
                "\nDeleting. Closing this screen does not cancel accepted work.")),
            Phase::Failed(error) => {
                let uncertain = error.details.as_ref().and_then(|details| details.get("deletion_outcome_uncertain"))
                    .and_then(Value::as_bool) == Some(true);
                let message = &error.message;
                text.push_str(&match (language, uncertain) {
                    (UiLanguage::Ja, true) => format!("\n削除結果は未確認です: {message}\n閉じてから再度選んでください。"),
                    (UiLanguage::Ja, false) => format!("\n削除できません: {message}\n閉じてから再度選んでください。"),
                    (UiLanguage::En, true) => format!("\nDeletion outcome is unknown: {message}\nClose this screen and select the target again."),
                    (UiLanguage::En, false) => format!("\nCould not delete: {message}\nClose this screen and select the target again."),
                });
            }
            Phase::InvalidPreview => text.push_str(local!(language,
                "\n削除できません: 削除影響の対象が一致しません。もう一度開いてください\n閉じてから再度選んでください。",
                "\nCould not delete: The deletion preview target does not match. Open it again.\nClose this screen and select the target again.")),
            Phase::Stale(None) => text.push_str(local!(language,
                "\n確認が古くなりました。閉じて再確認してください。Y は使えません。",
                "\nThe preview is stale. Close and review again. Y is disabled.")),
            Phase::Ready(value) | Phase::Stale(Some(value)) => {
                let number = |pointer: &str| value.pointer(pointer).and_then(Value::as_u64)
                    .map_or_else(|| local!(language, "不明", "unknown").to_owned(), |value| value.to_string());
                text.push_str(if matches!(self.phase, Phase::Stale(_)) {
                    local!(language, "\n確認が古くなりました。閉じて再確認してください。Y は使えません。", "\nThe preview is stale. Close and review again. Y is disabled.")
                } else if self.args.preview_marker.is_none() {
                    local!(language, "\n確認の目印を取得できません。対象範囲を狭めて再確認してください。Y は使えません。", "\nNo preview marker is available. Narrow the scope and review again. Y is disabled.")
                } else if self.can_execute() {
                    local!(language, "\n削除できます。Y で実行します。", "\nDeletion is available. Press Y to execute.")
                } else {
                    local!(language, "\n削除できません。作業を停止してから再度確認してください。", "\nDeletion is not available. Stop active work and review again.")
                });
                let selected = number("/selection/thread_count");
                let descendants = number("/selection/descendant_thread_count");
                let running = number("/blockers/live_run_count");
                let stopping = number("/blockers/stopping_run_count");
                let shared = number("/references/shared_path_nodes");
                let sources = number("/references/completed_replacement_source_nodes");
                let attachments = number("/references/attachment_reference_count");
                text.push_str(&match language {
                    UiLanguage::Ja => format!("\n対象会話: {selected} | 子の会話: {descendants}\n動作中: {running} | 停止中: {stopping}\n共有されている履歴: {shared}\n置換の出典: {sources} | 添付参照: {attachments}"),
                    UiLanguage::En => format!("\nSelected conversations: {selected} | Descendants: {descendants}\nRunning: {running} | Stopping: {stopping}\nShared history: {shared}\nReplacement sources: {sources} | Attachment references: {attachments}"),
                });
                if let Some(runs) = value.pointer("/blockers/runs").and_then(Value::as_array) {
                    for run in runs.iter().take(50) {
                        if let Some(id) = run.get("id").and_then(Value::as_str) {
                            let state = run.get("state").and_then(Value::as_str).unwrap_or(local!(language, "不明", "unknown"));
                            text.push_str(&match language {
                                UiLanguage::Ja => format!("\n  作業 {id} | {state}"),
                                UiLanguage::En => format!("\n  Run {id} | {state}"),
                            });
                        }
                    }
                }
                text.push_str(local!(language,
                    "\n\n削除直前にも状態を確認します。共有元の履歴は即時に破棄されません。",
                    "\n\nState is checked again immediately before deletion. Shared source history is not immediately discarded."));
            }
        }
        text
    }
}
impl UiState {
    fn deletion_action(&self, dialog: &DeletionDialog, execute: bool) -> UiAction {
        let mut known = self
            .views
            .values()
            .filter_map(|view| view.thread_id.clone())
            .chain(
                self.thread_browser
                    .items()
                    .iter()
                    .filter(|thread| !thread.thread_id.starts_with("draft:"))
                    .map(|thread| thread.thread_id.clone()),
            )
            .collect::<BTreeSet<_>>();
        known.insert(dialog.args.thread_id.clone());
        UiAction::ThreadDeletion {
            request_id: dialog.request_id,
            args: dialog.args.clone(),
            execute,
            known_thread_ids: known.into_iter().collect(),
        }
    }
    pub(super) fn begin_thread_deletion(&mut self, thread: String) -> Vec<UiAction> {
        self.begin_thread_deletion_scope(thread, false)
    }
    pub(super) fn begin_thread_deletion_scope(
        &mut self,
        thread: String,
        descendants: bool,
    ) -> Vec<UiAction> {
        if thread.starts_with("draft:") {
            let message = local!(
                self.config.ui.language,
                "未保存の下書きは会話の削除対象ではありません",
                "An unsaved draft is not a conversation to delete"
            );
            self.thread_browser.notice(message.into());
            return vec![];
        }
        if !self.thread_deletions_inflight.is_empty() {
            let message = local!(
                self.config.ui.language,
                "削除を実行中です。結果を待ってから再操作してください",
                "Deletion is in progress. Wait for its result before trying again"
            )
            .to_owned();
            self.thread_browser.notice(message.clone());
            self.active_view_mut().status = message;
            return vec![];
        }
        self.next_thread_deletion_request = self.next_thread_deletion_request.wrapping_add(1);
        let title = self
            .thread_browser
            .items()
            .iter()
            .find(|item| item.thread_id == thread)
            .map(|item| crate::presentation::sanitize_plain(&item.title, 0).0)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| local!(self.config.ui.language, "タイトルなし", "Untitled").into());
        let dialog = DeletionDialog {
            language: self.config.ui.language,
            title,
            captured_at: None,
            request_id: self.next_thread_deletion_request,
            args: ThreadDeleteArgs {
                preview_marker: None,
                thread_id: thread,
                descendants,
            },
            phase: Phase::Loading,
        };
        let action = self.deletion_action(&dialog, false);
        self.confirmation_scroll = 0;
        self.set_confirmation(Confirmation::ThreadDeletion(dialog));
        self.dirty = true;
        vec![action]
    }
    pub(super) fn thread_deletion_key(&mut self, key: KeyEvent) -> Vec<UiAction> {
        if key.kind == KeyEventKind::Release {
            return vec![];
        }
        if !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Esc | KeyCode::Char('n' | 'N'))
        {
            self.confirmation = None;
            self.confirmation_scroll = 0;
            self.dirty = true;
            return vec![];
        }
        let page = self.confirmation.as_ref().map_or(1, |dialog| {
            dialog.page(popup::viewport(self.viewport), self.config.ui.language)
        });
        if let Some(scroll) = popup::reader_key(key.code, self.confirmation_scroll, page) {
            self.confirmation_scroll = scroll;
            self.dirty = true;
        }
        let Some(Confirmation::ThreadDeletion(mut dialog)) = self.confirmation.take() else {
            return vec![];
        };
        let mut execute = None;
        if key.kind == KeyEventKind::Press
            && !matches!(dialog.phase, Phase::Executing)
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char('d' | 'D' | ' '))
        {
            dialog.args.descendants = !dialog.args.descendants;
            dialog.args.preview_marker = None;
            dialog.captured_at = None;
            dialog.phase = Phase::Loading;
            execute = Some(false);
        } else if !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char('y' | 'Y'))
            && dialog.can_execute()
        {
            dialog.phase = Phase::Executing;
            execute = Some(true);
        }
        let action = execute.map(|execute| {
            self.next_thread_deletion_request = self.next_thread_deletion_request.wrapping_add(1);
            dialog.request_id = self.next_thread_deletion_request;
            if execute {
                self.thread_deletions_inflight
                    .insert(dialog.args.thread_id.clone(), dialog.request_id);
            }
            self.confirmation_scroll = 0;
            self.deletion_action(&dialog, execute)
        });
        self.set_confirmation(Confirmation::ThreadDeletion(dialog));
        self.dirty = true;
        action.into_iter().collect()
    }
    pub(super) fn apply_thread_deletion(
        &mut self,
        request_id: u64,
        args: ThreadDeleteArgs,
        execute: bool,
        result: Result<ThreadDeletionReply, crate::protocol::ApiError>,
    ) {
        if execute && self.thread_deletions_inflight.get(&args.thread_id) == Some(&request_id) {
            self.thread_deletions_inflight.remove(&args.thread_id);
        }
        let result = match result {
            Ok(ThreadDeletionReply::DeletedWithWarning { ids, warning }) if execute => {
                self.remove_deleted_threads(&ids);
                let message = match self.config.ui.language {
                    UiLanguage::En => format!("Deleted, but cleanup failed: {warning}"),
                    UiLanguage::Ja => format!("削除しました。後片付けに失敗しました: {warning}"),
                };
                self.thread_browser.notice(message.clone());
                self.active_view_mut().status = message;
                self.dirty = true;
                return;
            }
            other => other,
        };
        let matches = matches!(&self.confirmation,Some(Confirmation::ThreadDeletion(dialog)) if dialog.matches(request_id,&args));
        match result {
            Ok(ThreadDeletionReply::Deleted(ids)) if execute => {
                // Durable success owns cleanup even after N/Esc or a different
                // dialog was opened. It must never reopen a closed overlay.
                self.remove_deleted_threads(&ids);
                if matches {
                    self.confirmation = None;
                    self.confirmation_scroll = 0;
                }
                let message = super::i18n::Message::DeletedConversations { count: ids.len() }
                    .render(self.config.ui.language);
                self.thread_browser.notice(message);
            }
            Ok(ThreadDeletionReply::Preview(value, captured_at)) if !execute && matches => {
                if let Some(Confirmation::ThreadDeletion(dialog)) = &mut self.confirmation
                    && matches!(dialog.phase, Phase::Loading)
                {
                    dialog.captured_at = Some(captured_at);
                    dialog.args.preview_marker = value
                        .get("preview_marker")
                        .cloned()
                        .and_then(|marker| serde_json::from_value(marker).ok());
                    dialog.phase = if value.get("thread_id").and_then(Value::as_str)
                        == Some(args.thread_id.as_str())
                        && value.get("descendants").and_then(Value::as_bool)
                            == Some(args.descendants)
                    {
                        Phase::Ready(value)
                    } else {
                        Phase::InvalidPreview
                    };
                }
            }
            Err(error) => {
                if execute && matches && error.code == crate::protocol::ErrorCode::PreviewStale {
                    // A stale compare never retries the mutation: get a new
                    // snapshot/request fence, then require a fresh confirmation.
                    self.next_thread_deletion_request =
                        self.next_thread_deletion_request.wrapping_add(1);
                    if let Some(Confirmation::ThreadDeletion(mut dialog)) = self.confirmation.take()
                    {
                        dialog.request_id = self.next_thread_deletion_request;
                        dialog.args.preview_marker = None;
                        dialog.captured_at = None;
                        dialog.phase = Phase::Loading;
                        let action = self.deletion_action(&dialog, false);
                        self.deferred_actions.push(action);
                        self.set_confirmation(Confirmation::ThreadDeletion(dialog));
                    }
                    let message = local!(
                        self.config.ui.language,
                        "削除対象が変わりました。再確認してください",
                        "The deletion target changed. Review it again"
                    );
                    self.thread_browser.notice(message.into());
                    self.dirty = true;
                    return;
                }
                if execute {
                    let message = failure_text(&error, self.config.ui.language);
                    self.thread_browser.notice(message.clone());
                    self.active_view_mut().status = message;
                }
                if matches
                    && let Some(Confirmation::ThreadDeletion(dialog)) = &mut self.confirmation
                    && (execute || matches!(dialog.phase, Phase::Loading))
                {
                    dialog.phase = Phase::Failed(error);
                }
            }
            _ => {}
        }
        if matches {
            self.confirmation_cache.clear();
        }
        self.dirty = true;
    }
    fn remove_deleted_threads(&mut self, ids: &[String]) {
        let deleted = ids.iter().cloned().collect::<BTreeSet<_>>();
        let keys = self
            .views
            .iter()
            .filter(|(_, view)| {
                view.thread_id
                    .as_ref()
                    .is_some_and(|id| deleted.contains(id))
                    || view
                        .draft
                        .edit
                        .as_ref()
                        .is_some_and(|edit| deleted.contains(&edit.thread_id))
            })
            .map(|(key, _)| key.clone())
            .collect::<BTreeSet<_>>();
        let draft_ids = self
            .views
            .iter()
            .filter(|(key, _)| keys.contains(*key))
            .map(|(_, view)| view.draft.draft_id.clone())
            .collect::<BTreeSet<_>>();
        if keys.contains(&self.active) {
            // Do not call new_chat: it persists the old active draft. Activate
            // a fresh view before removing anything the view accessor needs.
            let fresh = ThreadView::new(&self.config, self.startup_cwd.clone());
            let key = fresh.draft.draft_id.clone();
            self.views.insert(key.clone(), fresh);
            self.active = key;
            self.operations = None;
            self.models = None;
            self.details = None;
            self.management.close();
            self.thread_browser.close();
            self.deferred_actions.push(UiAction::ActivateDraft {
                draft_id: self.view().draft.draft_id.clone(),
            });
        }
        self.views.retain(|key, _| !keys.contains(key));
        self.autosave.forget_views(&keys);
        self.deferred_actions
            .retain(|action| !targets_deleted(action, &deleted, &draft_ids));
        self.render_pending
            .retain(|(thread, _, _, _, _, _)| !deleted.contains(thread));
        self.layout_cache = HistoryLayoutCache::default();
        self.visible_links.clear();
        let mut list_ids = ids.to_vec();
        list_ids.extend(keys.iter().map(|key| format!("draft:{key}")));
        self.thread_browser.remove_deleted(&list_ids);
        if matches!(&self.confirmation,Some(Confirmation::ThreadDeletion(dialog)) if deleted.contains(&dialog.args.thread_id))
        {
            self.confirmation = None;
        }
    }
}
fn targets_deleted(
    action: &UiAction,
    threads: &BTreeSet<String>,
    drafts: &BTreeSet<String>,
) -> bool {
    match action {
        UiAction::ManagementIntent { context, action } => {
            context
                .owner_thread
                .as_ref()
                .is_some_and(|thread| threads.contains(thread))
                || drafts.contains(&context.owner_draft)
                || targets_deleted(action, threads, drafts)
        }
        UiAction::SubmittedCommand { submission, action } => {
            drafts.contains(&submission.draft_id) || targets_deleted(action, threads, drafts)
        }
        UiAction::Send {
            draft_id,
            thread_id,
            ..
        }
        | UiAction::PasteImage {
            draft_id,
            thread_id,
            ..
        } => {
            drafts.contains(draft_id)
                || thread_id
                    .as_ref()
                    .is_some_and(|thread| threads.contains(thread))
        }
        UiAction::SendEdited {
            draft_id,
            source_thread_id,
            ..
        } => drafts.contains(draft_id) || threads.contains(source_thread_id),
        UiAction::ClipboardDecision { draft_id, .. }
        | UiAction::DraftAttachments { draft_id, .. }
        | UiAction::ActivateDraft { draft_id }
        | UiAction::RestoreSnapshot { draft_id, .. } => drafts.contains(draft_id),
        UiAction::ClearToolObservation { thread_id, .. }
        | UiAction::RefreshTools { thread_id, .. }
        | UiAction::LoadReceived { thread_id, .. }
        | UiAction::RecallInput { thread_id, .. }
        | UiAction::LoadEditRequest { thread_id, .. }
        | UiAction::ReloadAttachments { thread_id }
        | UiAction::LoadHistoryWindow { thread_id, .. }
        | UiAction::RebaseHistory { thread_id, .. }
        | UiAction::SelectThread { thread_id, .. }
        | UiAction::LoadNodeWindow { thread_id, .. }
        | UiAction::LoadHistory { thread_id, .. }
        | UiAction::SaveDraft { thread_id, .. }
        | UiAction::Cancel { thread_id }
        | UiAction::Background { thread_id } => threads.contains(thread_id),
        UiAction::RebaseStream(request) => threads.contains(&request.thread_id),
        UiAction::ThreadDeletion { args, .. } => threads.contains(&args.thread_id),
        _ => false,
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn duplicate_preview_cannot_replace_the_original_evidence() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        let action = ui.begin_thread_deletion("thread".into()).pop().unwrap();
        let UiAction::ThreadDeletion {
            request_id, args, ..
        } = action
        else {
            panic!("expected preview request");
        };
        let preview = |count| {
            ThreadDeletionReply::Preview(
                json!({
                    "thread_id": "thread", "descendants": false,
                    "deletable_now": true, "selection": {"thread_count": count}
                }),
                chrono::Utc::now(),
            )
        };
        ui.apply_thread_deletion(request_id, args.clone(), false, Ok(preview(1)));
        ui.apply_thread_deletion(request_id, args.clone(), false, Ok(preview(99)));
        ui.apply_thread_deletion(
            request_id,
            args,
            false,
            Err(crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::InvalidState,
                "late failure",
            )),
        );
        let Some(Confirmation::ThreadDeletion(dialog)) = &ui.confirmation else {
            panic!("expected deletion dialog");
        };
        let Phase::Ready(value) = &dialog.phase else {
            panic!("expected ready preview");
        };
        assert_eq!(value.pointer("/selection/thread_count"), Some(&json!(1)));
    }

    #[test]
    fn malformed_first_preview_cannot_be_replaced_by_a_duplicate() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        let action = ui.begin_thread_deletion("thread".into()).pop().unwrap();
        let UiAction::ThreadDeletion {
            request_id, args, ..
        } = action
        else {
            panic!("expected preview request");
        };
        ui.apply_thread_deletion(
            request_id,
            args.clone(),
            false,
            Ok(ThreadDeletionReply::Preview(
                json!({"thread_id": "other", "descendants": false}),
                chrono::Utc::now(),
            )),
        );
        ui.apply_thread_deletion(
            request_id,
            args,
            false,
            Ok(ThreadDeletionReply::Preview(
                json!({"thread_id": "thread", "descendants": false}),
                chrono::Utc::now(),
            )),
        );
        assert!(matches!(&ui.confirmation,
            Some(Confirmation::ThreadDeletion(dialog)) if matches!(dialog.phase, Phase::InvalidPreview)));
    }
}

#[cfg(test)]
mod invalidation_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn changed_deletion_preserves_evidence_disables_y_and_rejects_late_preview() {
        let mut config = Config::default();
        config.ui.language = UiLanguage::Ja;
        let mut ui = UiState::new(&config, std::env::temp_dir());
        let action = ui.begin_thread_deletion("thread".into()).pop().unwrap();
        let UiAction::ThreadDeletion {
            request_id, args, ..
        } = action
        else {
            panic!("preview")
        };
        let value = json!({"thread_id":"thread", "descendants":false, "deletable_now":true,
            "selection":{"thread_count":1}});
        let captured_at = chrono::DateTime::parse_from_rfc3339("2026-10-30T04:05:06Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        ui.apply_thread_deletion(
            request_id,
            args.clone(),
            false,
            Ok(ThreadDeletionReply::Preview(value.clone(), captured_at)),
        );
        ui.apply(UiUpdate::ConfirmationSourcesChanged);
        ui.apply_thread_deletion(
            request_id,
            args,
            false,
            Ok(ThreadDeletionReply::Preview(
                json!({"thread_id":"thread", "descendants":false, "deletable_now":true}),
                chrono::Utc::now(),
            )),
        );
        let Some(Confirmation::ThreadDeletion(dialog)) = &ui.confirmation else {
            panic!("dialog")
        };
        assert!(matches!(&dialog.phase, Phase::Stale(Some(saved)) if saved == &value));
        assert!(dialog.text().contains("古く"));
        assert_eq!(dialog.captured_at, Some(captured_at));
        assert!(dialog.text().contains("2026-10-30T04:05:06Z"));
        assert!(dialog.footer().contains("Y 使用不可"));
        assert!(
            ui.thread_deletion_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
                .is_empty()
        );
        let actions = ui.thread_deletion_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(matches!(
            actions.as_slice(),
            [UiAction::ThreadDeletion { execute: false, .. }]
        ));
    }
}

#[cfg(test)]
mod marker_entry_tests {
    use super::*;
    #[test]
    fn operation_form_also_requests_review_instead_of_an_unmarked_confirmation() {
        let mut ui = UiState::new(&crate::config::Config::default(), std::env::temp_dir());
        let actions = ui.execute_operation(Command::ThreadDelete(ThreadDeleteArgs {
            thread_id: "root".into(),
            descendants: true,
            preview_marker: None,
        }));
        assert!(
            matches!(&actions[..],[UiAction::ThreadDeletion{args,execute:false,..}] if args.thread_id=="root" && args.descendants && args.preview_marker.is_none())
        );
        assert!(
            matches!(&ui.confirmation,Some(Confirmation::ThreadDeletion(dialog)) if !dialog.can_execute())
        );
    }
}

#[cfg(test)]
mod language_rendering_tests {
    use super::*;
    use crate::config::UiLanguage;

    fn show_ready(ui: &mut UiState) {
        let action = ui
            .begin_thread_deletion("thread-fixture".into())
            .pop()
            .unwrap();
        let UiAction::ThreadDeletion {
            request_id, args, ..
        } = action
        else {
            panic!("preview request")
        };
        let body = serde_json::json!({
            "thread_id": "thread-fixture", "descendants": false,
            "deletable_now": true, "selection": {"thread_count": 1},
            "preview_marker": {"thread_count": 1, "ids_hash": "0".repeat(64)}
        });
        ui.apply_thread_deletion(
            request_id,
            args,
            false,
            Ok(ThreadDeletionReply::Preview(body, chrono::Utc::now())),
        );
    }

    #[test]
    fn ready_deletion_review_keeps_destructive_action_explicit() {
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["thread-fixture", "Y Delete"],
                UiLanguage::Ja => &["thread-fixture", "Y 削除"],
            }
        }
        super::super::i18n::verify_screen(show_ready, expected);
    }

    #[test]
    fn stale_deletion_review_disables_destructive_action() {
        fn configure(ui: &mut UiState) {
            show_ready(ui);
            ui.apply(UiUpdate::ConfirmationSourcesChanged);
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["thread-fixture", "Y disabled"],
                UiLanguage::Ja => &["thread-fixture", "Y 使用不可"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn deletion_failure_preserves_provider_message_and_blocks_execution() {
        fn configure(ui: &mut UiState) {
            let action = ui
                .begin_thread_deletion("thread-fixture".into())
                .pop()
                .unwrap();
            let UiAction::ThreadDeletion {
                request_id, args, ..
            } = action
            else {
                panic!("preview request")
            };
            let error = crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::InvalidState,
                "PROVIDER-ERROR: opaque detail",
            );
            ui.apply_thread_deletion(request_id, args, false, Err(error));
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["PROVIDER-ERROR: opaque detail", "Y disabled"],
                UiLanguage::Ja => &["PROVIDER-ERROR: opaque detail", "Y 使用不可"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn loading_deletion_review_shows_scope_and_disabled_action_in_both_languages() {
        fn configure(ui: &mut UiState) {
            let _ = ui.begin_thread_deletion("thread-fixture".into());
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["thread-fixture", "Checking deletion impact", "Y disabled"],
                UiLanguage::Ja => &["thread-fixture", "削除影響を確認中", "Y 使用不可"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }
}
