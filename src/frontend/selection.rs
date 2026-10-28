//! Frontend observation context, separate from UI views and durable Thread state.
use super::{ApiError, UiUpdate, UpdateTarget, activity, status, update};
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;
#[derive(Default)]
pub(super) struct Selection {
    pub(super) generations: HashMap<String, u64>,
    pub(super) active_thread: Option<String>,
    pub(super) active_draft: Option<String>,
    pub(super) progress_running: bool,
    pub(super) progress_epoch: u64,
    pub(super) cancel: CancellationToken,
}
impl Selection {
    fn refresh(&mut self) {
        self.progress_epoch = self.progress_epoch.wrapping_add(1);
        self.progress_running = true;
    }
    fn replace_token(&mut self) {
        self.cancel.cancel();
        self.cancel = CancellationToken::new();
    }
    pub(super) fn activate_draft(&mut self, draft: String) {
        self.progress_epoch = self.progress_epoch.wrapping_add(1);
        self.replace_token();
        self.active_draft = Some(draft);
        self.active_thread = None;
        self.progress_running = false;
    }
    pub(super) fn select_thread(&mut self, thread: String, generation: u64) {
        self.refresh();
        self.replace_token();
        self.active_thread = Some(thread.clone());
        self.active_draft = None;
        self.generations.insert(thread, generation);
    }
    pub(super) fn sent(&mut self, thread: &str, draft: &str) {
        if self.active_draft.as_deref() == Some(draft) {
            self.active_thread = Some(thread.to_owned());
            self.refresh();
        }
        self.generations.entry(thread.to_owned()).or_insert(0);
    }
    pub(super) fn changed(&mut self, thread: &str) {
        if self.active_thread.as_deref() == Some(thread) {
            self.refresh();
        }
    }
    fn accepts(&self, thread: &str, generation: u64, epoch: u64) -> bool {
        self.active_thread.as_deref() == Some(thread)
            && epoch == self.progress_epoch
            && self.generations.get(thread) == Some(&generation)
    }
    pub(super) async fn publish_activity(
        &mut self,
        updates: &impl UpdateTarget,
        thread: String,
        generation: u64,
        epoch: u64,
        result: Result<activity::Projection, ApiError>,
    ) {
        if !self.accepts(&thread, generation, epoch) {
            return;
        }
        match result {
            Ok(projection) => {
                self.progress_running = projection.poll_running;
                update(updates, projection.progress_update(&thread)).await;
                update(
                    updates,
                    projection.into_activity_update(&thread, generation, epoch),
                )
                .await;
            }
            Err(error) => {
                self.progress_running = false;
                update(
                    updates,
                    UiUpdate::ActivityUnavailable {
                        thread_id: thread.clone(),
                        generation,
                        epoch,
                    },
                )
                .await;
                status(updates, Some(thread), error).await;
            }
        }
    }
}
