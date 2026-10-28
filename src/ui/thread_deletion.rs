//! Request-scoped deletion review on the existing confirmation overlay.
use super::*;
use serde_json::Value;
pub(super) const FOOTER: &str =
    "D / Space 子の会話も削除を切替 · N / Esc 戻る\nY 削除 · ↑↓/PgUp/PgDn 閲覧";
#[derive(Clone, Debug)]
enum Phase {
    Loading,
    Ready(Value),
    Executing,
    Failed(crate::protocol::ApiError),
}
#[derive(Clone, Debug)]
pub(super) struct DeletionDialog {
    request_id: u64,
    args: ThreadDeleteArgs,
    title: String,
    phase: Phase,
}
impl DeletionDialog {
    fn matches(&self, request_id: u64, args: &ThreadDeleteArgs) -> bool {
        self.request_id == request_id
            && self.args.thread_id == args.thread_id
            && self.args.descendants == args.descendants
    }
    fn can_execute(&self) -> bool {
        matches!(&self.phase,Phase::Ready(value) if value.get("deletable_now").and_then(Value::as_bool)==Some(true))
    }
    pub(super) fn text(&self) -> String {
        let mut text = format!(
            "会話の削除確認\n対象: {}\n会話ID: {}\n子の会話も削除: {}\n",
            self.title,
            self.args.thread_id,
            if self.args.descendants {
                "する"
            } else {
                "しない"
            }
        );
        match &self.phase {
            Phase::Loading => text.push_str("\n削除影響を確認中… Y はまだ使えません。"),
            Phase::Executing => {
                text.push_str("\n削除を実行中… 閉じても受理済みの処理は継続します。")
            }
            Phase::Failed(error) => text.push_str(&format!(
                "\n削除できません: {}\n閉じてから再度選んでください。",
                error.message
            )),
            Phase::Ready(value) => {
                let number = |pointer: &str| {
                    value
                        .pointer(pointer)
                        .and_then(Value::as_u64)
                        .map_or_else(|| "不明".to_owned(), |value| value.to_string())
                };
                text.push_str(if self.can_execute() {
                    "\n削除できます。Y で実行します。"
                } else {
                    "\n削除できません。作業を停止してから再度確認してください。"
                });
                text.push_str(&format!("\n対象会話: {} · 子の会話: {}\n動作中: {} · 停止中: {}\n共有されている履歴: {}\n置換の出典: {} · 添付参照: {}",
                    number("/selection/thread_count"),number("/selection/descendant_thread_count"),number("/blockers/live_run_count"),number("/blockers/stopping_run_count"),number("/references/shared_path_nodes"),number("/references/completed_replacement_source_nodes"),number("/references/attachment_reference_count")));
                if let Some(runs) = value.pointer("/blockers/runs").and_then(Value::as_array) {
                    for run in runs.iter().take(50) {
                        if let Some(id) = run.get("id").and_then(Value::as_str) {
                            text.push_str(&format!(
                                "\n  作業 {} · {}",
                                id,
                                run.get("state").and_then(Value::as_str).unwrap_or("不明")
                            ));
                        }
                    }
                }
                text.push_str(
                    "\n\n削除直前にも状態を確認します。共有元の履歴は即時に破棄されません。",
                );
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
        if thread.starts_with("draft:") {
            self.thread_browser
                .notice("未保存の下書きは会話の削除対象ではありません".into());
            return vec![];
        }
        if !self.thread_deletions_inflight.is_empty() {
            let message = "削除を実行中です。結果を待ってから再操作してください".to_owned();
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
            .unwrap_or_else(|| "タイトルなし".into());
        let dialog = DeletionDialog {
            title,
            request_id: self.next_thread_deletion_request,
            args: ThreadDeleteArgs {
                thread_id: thread,
                descendants: false,
            },
            phase: Phase::Loading,
        };
        let action = self.deletion_action(&dialog, false);
        self.confirmation_scroll = 0;
        self.confirmation = Some(Confirmation::ThreadDeletion(dialog));
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
        let page = self
            .confirmation
            .as_ref()
            .map_or(1, |dialog| dialog.page(popup::viewport(self.viewport)));
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
        self.confirmation = Some(Confirmation::ThreadDeletion(dialog));
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
                let message = format!("削除しました。後片付けに失敗しました: {warning}");
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
                self.thread_browser
                    .notice(format!("{} 件の会話を削除しました", ids.len()));
            }
            Ok(ThreadDeletionReply::Preview(value)) if !execute && matches => {
                if let Some(Confirmation::ThreadDeletion(dialog)) = &mut self.confirmation {
                    dialog.phase = if value.get("thread_id").and_then(Value::as_str)
                        == Some(args.thread_id.as_str())
                        && value.get("descendants").and_then(Value::as_bool)
                            == Some(args.descendants)
                    {
                        Phase::Ready(value)
                    } else {
                        Phase::Failed(crate::protocol::ApiError::new(
                            crate::protocol::ErrorCode::InvalidState,
                            "削除影響の対象が一致しません。もう一度開いてください",
                        ))
                    };
                }
            }
            Err(error) => {
                if execute {
                    let message = format!("削除できません: {}", error.message);
                    self.thread_browser.notice(message.clone());
                    self.active_view_mut().status = message;
                }
                if matches
                    && let Some(Confirmation::ThreadDeletion(dialog)) = &mut self.confirmation
                {
                    dialog.phase = Phase::Failed(error);
                }
            }
            _ => {}
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
        UiAction::RefreshTools { thread_id, .. }
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
