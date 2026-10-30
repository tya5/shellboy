//! UI-owned save responsibility: lightweight signatures, bounded work per turn.
use super::*;
use std::collections::{HashSet, VecDeque};
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Signature {
    draft_id: String,
    revision: u64,
    cursor: usize,
    anchor: ViewAnchor,
    follow: bool,
}
impl Signature {
    fn of(view: &ThreadView) -> Self {
        Self {
            draft_id: view.draft.draft_id.clone(),
            revision: view.draft.revision,
            cursor: view.draft.cursor,
            anchor: view.anchor.clone(),
            follow: view.follow,
        }
    }
}
#[derive(Default)]
pub(super) struct Autosave {
    dirty: VecDeque<String>,
    queued: HashSet<String>,
    admitted: BTreeMap<String, Signature>,
}
pub(super) struct Request {
    key: String,
    signature: Signature,
    pub action: UiAction,
}
impl Autosave {
    pub(super) fn forget_views(&mut self, keys: &BTreeSet<String>) {
        self.dirty.retain(|key| !keys.contains(key));
        self.queued.retain(|key| !keys.contains(key));
        self.admitted.retain(|key, _| !keys.contains(key));
    }
    pub(super) fn pending(&self) -> bool {
        !self.dirty.is_empty()
    }
    pub(super) fn touch(&mut self, key: &str) {
        if self.queued.insert(key.to_owned()) {
            self.dirty.push_back(key.to_owned());
        }
    }
}
pub(super) fn saveable(view: &ThreadView) -> bool {
    // A newly selected empty placeholder is not an edited empty draft. Saving
    // it before the read completes would mask the existing persisted draft.
    !view.draft_restore_pending
        || view.draft.revision != 0
        || !view.draft.text.is_empty()
        || !view.draft.attachments.is_empty()
        || !view.pending.is_empty()
}
impl UiState {
    pub(super) fn save_requests(&mut self) -> Vec<Request> {
        let mut requests = Vec::new();
        // Bound signature work as well as snapshot copies. Retained views own
        // their source; this queue owns only one ID per touched view.
        for _ in 0..8 {
            let Some(key) = self.autosave.dirty.pop_front() else {
                break;
            };
            self.autosave.queued.remove(&key);
            let Some(view) = self.views.get(&key) else {
                self.autosave.admitted.remove(&key);
                continue;
            };
            let Some(thread_id) = &view.thread_id else {
                continue;
            };
            if !saveable(view) {
                continue;
            }
            let signature = Signature::of(view);
            if view.save_retry || self.autosave.admitted.get(&key) != Some(&signature) {
                requests.push(Request {
                    key,
                    signature,
                    action: UiAction::SaveDraft {
                        thread_id: thread_id.clone(),
                        draft: view.draft.clone(),
                        anchor: view.anchor.clone(),
                        follow: view.follow,
                    },
                });
            }
        }
        requests
    }
    pub(super) fn save_admitted(&mut self, key: String, signature: Signature) {
        if let Some(view) = self.views.get_mut(&key) {
            // Queue admission owns this snapshot, not any later editing state.
            if Signature::of(view) == signature {
                view.save_retry = false;
            }
        }
        self.autosave.admitted.insert(key, signature);
    }
    pub(super) fn submit_saves(&mut self, actions: &actions::ActionSender) {
        for Request {
            key,
            signature,
            action,
        } in self.save_requests()
        {
            match actions.try_send(action) {
                Ok(()) => self.save_admitted(key, signature),
                Err(error) => self.action_rejected(&error.into_inner()),
            }
        }
    }
    pub(super) fn retry_save(&mut self, thread: &str, message: &str) {
        if let Some((key, view)) = self
            .views
            .iter_mut()
            .find(|(_, view)| view.thread_id.as_deref() == Some(thread))
        {
            view.save_retry = true;
            view.status = message.into();
            self.autosave.touch(key);
        }
        self.dirty = true;
    }
    pub(super) fn persist_all(&self) -> Vec<UiAction> {
        self.views
            .iter()
            .filter(|(key, view)| {
                saveable(view)
                    && (view.save_retry
                        || self.autosave.admitted.get(*key) != Some(&Signature::of(view)))
            })
            .filter_map(|(_, view)| {
                view.thread_id.as_ref().map(|thread| UiAction::SaveDraft {
                    thread_id: thread.clone(),
                    draft: view.draft.clone(),
                    anchor: view.anchor.clone(),
                    follow: view.follow,
                })
            })
            .collect()
    }
    pub(super) fn touch_save_update(&mut self, update: &UiUpdate) {
        // These mutate drafts outside active_view_mut/view_for. Queue an ID,
        // never a body, and use the current snapshot when the save becomes due.
        let draft = match update {
            UiUpdate::AgentsSelected { draft_id, .. }
            | UiUpdate::Receipt { draft_id, .. }
            | UiUpdate::ClipboardAttachment { draft_id, .. }
            | UiUpdate::RestoredSnapshot { draft_id, .. } => Some(draft_id.as_str()),
            UiUpdate::CommandOutcome { submission, .. } => Some(submission.draft_id.as_str()),
            _ => None,
        };
        if let Some(draft) = draft
            && let Some((key, _)) = self.views.iter().find(|(_, view)| {
                view.draft.draft_id == draft
                    || view.pending.values().any(|p| p.snapshot.draft_id == draft)
            })
        {
            self.autosave.touch(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(action: &UiAction) -> (&str, &str) {
        let UiAction::SaveDraft {
            thread_id, draft, ..
        } = action
        else {
            panic!("expected save")
        };
        (thread_id, &draft.text)
    }
    #[test]
    fn inactive_rejection_and_late_failure_retry_current_snapshot_without_editing_active_input() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("first".into());
        ui.insert("older", false);
        let first_key = ui.active.clone();
        let first = ui.save_requests().remove(0);
        ui.save_admitted(first.key, first.signature);
        ui.insert(" latest", false);
        let rejected = ui.select_thread("second".into()).remove(0);
        ui.insert("active input", false);
        ui.action_rejected(&rejected);
        let requests = ui.save_requests();
        assert!(
            requests
                .iter()
                .any(|r| text(&r.action) == ("first", "older latest"))
        );
        assert_eq!(ui.view().draft.text, "active input");
        for request in requests {
            ui.save_admitted(request.key, request.signature);
        }
        assert!(!ui.views[&first_key].save_retry);
        // A late failure requests the current snapshot, never the old body.
        ui.apply(UiUpdate::DraftSaveFailed {
            thread_id: "first".into(),
            message: "old save failed".into(),
        });
        let retry = ui.save_requests();
        assert_eq!(retry.len(), 1);
        assert_eq!(text(&retry[0].action), ("first", "older latest"));
        assert_eq!(ui.view().draft.text, "active input");
    }
    #[test]
    fn dirty_ids_are_finite_fair_and_unchanged_signatures_do_not_copy_again() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        for i in 0..25 {
            ui.select_thread(format!("thread-{i}"));
            ui.insert("本文", false);
        }
        let mut visited = HashSet::new();
        while !ui.autosave.dirty.is_empty() {
            let requests = ui.save_requests();
            assert!(requests.len() <= 8);
            for request in requests {
                visited.insert(text(&request.action).0.to_owned());
                ui.save_admitted(request.key, request.signature);
            }
        }
        assert_eq!(visited.len(), 25);
        assert!(
            ui.persist_all().is_empty(),
            "already owned saves need no duplicate shutdown bodies"
        );
        for _ in 0..1000 {
            ui.view_mut().status = "status only".into();
        }
        assert_eq!(ui.autosave.dirty.len(), 1);
        assert!(ui.save_requests().is_empty());
        assert!(ui.autosave.dirty.is_empty());
        for _ in 0..1000 {
            ui.animation_tick(Instant::now());
        }
        assert!(
            !ui.autosave.pending(),
            "idle display clocks must not schedule saves"
        );
    }
    #[test]
    fn unresolved_empty_placeholder_is_not_a_new_draft_but_edits_remain_saveable() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("not-read-yet".into());
        assert!(ui.save_requests().is_empty());
        assert!(ui.persist_all().is_empty());
        assert!(ui.persist_active().is_empty());
        ui.insert("edited before restore", false);
        assert_eq!(ui.save_requests().len(), 1);
        assert_eq!(ui.persist_all().len(), 1);
    }
    #[tokio::test]
    async fn full_admission_keeps_inactive_save_responsibility_until_next_opportunity() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("first".into());
        ui.insert("first", false);
        ui.select_thread("second".into());
        ui.insert("second", false);
        let (sender, mut receiver) = actions::channel(1);
        sender
            .try_send(UiAction::RenderWidth { width: 80 })
            .unwrap();
        ui.submit_saves(&sender);
        assert!(
            ui.views
                .values()
                .filter(|v| v.thread_id.is_some())
                .all(|v| v.save_retry)
        );
        receiver.recv().await.unwrap();
        ui.submit_saves(&sender);
        let first = receiver.recv().await.unwrap();
        ui.submit_saves(&sender);
        let second = receiver.recv().await.unwrap();
        let ids = [text(&first).0.to_owned(), text(&second).0.to_owned()]
            .into_iter()
            .collect::<HashSet<_>>();
        assert_eq!(ids, HashSet::from(["first".into(), "second".into()]));
        assert_eq!(ui.view().draft.text, "second");
    }
}
