//! Frontend observation context, separate from UI views and durable Thread state.
use super::{ApiError, UiUpdate, UpdateTarget, activity, status, update};
use crate::ui::ToolObservation;
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
    pub(super) tool_scope: Option<ToolObservation>,
    refresh_not_before: Option<tokio::time::Instant>,
    // Only the selected projection's bounded child rows are dependencies.
    live_children: Vec<String>,
}
impl Selection {
    pub(super) fn refresh_ready(&self, thread: &str, now: tokio::time::Instant) -> bool {
        self.active_thread.as_deref() != Some(thread)
            || self.refresh_not_before.is_none_or(|due| now >= due)
    }
    pub(super) fn refresh_started(&mut self, thread: &str, now: tokio::time::Instant) {
        if self.active_thread.as_deref() == Some(thread) {
            self.refresh_not_before = Some(now + std::time::Duration::from_millis(500));
        }
    }
    pub(super) fn refresh_deadline(
        &self,
        dirty: &std::collections::HashSet<String>,
        refreshing: &std::collections::HashSet<String>,
    ) -> Option<tokio::time::Instant> {
        let thread = self.active_thread.as_ref()?;
        if !dirty.contains(thread) || refreshing.contains(thread) {
            return None;
        }
        self.refresh_not_before
    }
    pub(super) fn observe_tools(
        &mut self,
        thread: &str,
        generation: u64,
        mut scope: ToolObservation,
    ) -> Result<bool, ApiError> {
        if self.active_thread.as_deref() != Some(thread)
            || self.generations.get(thread) != Some(&generation)
        {
            return Ok(false);
        }
        if scope.nodes.len() > 128
            || scope
                .nodes
                .iter()
                .any(|node| node.node_id.is_empty() || node.node_id.len() > 256)
        {
            return Err(ApiError::new(
                crate::protocol::ErrorCode::InputLimit,
                "Paused tool scope exceeds its bounded source allowance",
            ));
        }
        if self.tool_scope.as_ref().is_some_and(|old| {
            old.revision > scope.revision
                || (old.revision == scope.revision && old.view_revision == scope.view_revision)
        }) {
            return Ok(false);
        }
        scope.nodes = scope
            .nodes
            .iter()
            .filter_map(crate::ui::UiNode::tool_stub)
            .collect();
        self.tool_scope = Some(scope);
        Ok(true)
    }
    pub(super) fn clear_tools(&mut self, thread: &str, generation: u64) {
        if self.active_thread.as_deref() == Some(thread)
            && self.generations.get(thread) == Some(&generation)
        {
            self.tool_scope = None;
        }
    }
    fn refresh(&mut self) {
        self.progress_epoch = self.progress_epoch.wrapping_add(1);
        self.progress_running = true;
    }
    fn replace_token(&mut self) {
        self.tool_scope = None;
        self.live_children.clear();
        self.refresh_not_before = None;
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
    pub(super) fn dependent_parent(&self, changed: &str) -> Option<String> {
        self.live_children
            .iter()
            .any(|child| child == changed)
            .then(|| self.active_thread.clone())
            .flatten()
    }
    fn observe_live_children(&mut self, snapshot: &crate::ui::activity::ActivitySnapshot) {
        self.live_children = snapshot
            .live_work
            .as_ref()
            .map(|work| {
                work.rows
                    .iter()
                    .take(3)
                    .filter_map(|row| match &row.target {
                        crate::ui::live_work::LiveWorkTarget::ChildRun { thread_id, .. } => {
                            Some(thread_id.clone())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
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
                self.observe_live_children(&projection.snapshot);
                self.progress_running = projection.poll_running;
                update(updates, projection.progress_update(&thread)).await;
                update(
                    updates,
                    projection.into_activity_update(&thread, generation, epoch),
                )
                .await;
            }
            Err(error) => {
                self.live_children.clear();
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

#[cfg(test)]
mod refresh_scope_tests {
    use super::*;
    use crate::{
        presentation::{BlockKind, PresentationBlock, ToolPresentation},
        ui::UiNode,
    };
    fn scope(revision: u64, view_revision: u64) -> ToolObservation {
        ToolObservation {
            revision,
            view_revision,
            nodes: vec![UiNode {
                node_id: "call".into(),
                role: "assistant".into(),
                text: "BODY_MUST_NOT_BE_RETAINED".into(),
                total_chars: 25,
                presentation: crate::presentation::NodePresentation {
                    blocks: vec![PresentationBlock {
                        kind: BlockKind::ToolCall,
                        text: "raw block".into(),
                        tool: Some(ToolPresentation {
                            call_id: "saved-call".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ..Default::default()
            }],
        }
    }
    #[test]
    fn changed_bursts_share_one_start_deadline_and_switch_resets_it() {
        let mut selected = Selection::default();
        selected.select_thread("t".into(), 1);
        let start = tokio::time::Instant::now();
        assert!(selected.refresh_ready("t", start));
        selected.refresh_started("t", start);
        for _ in 0..100 {
            selected.changed("t");
        }
        let dirty = std::collections::HashSet::from(["t".into()]);
        assert_eq!(
            selected.refresh_deadline(&dirty, &Default::default()),
            Some(start + std::time::Duration::from_millis(500))
        );
        assert!(!selected.refresh_ready("t", start + std::time::Duration::from_millis(499)));
        assert!(selected.refresh_ready("t", start + std::time::Duration::from_millis(500)));
        assert!(selected.refresh_ready("hidden", start));
        assert!(
            selected
                .refresh_deadline(&dirty, &std::collections::HashSet::from(["t".into()]))
                .is_none()
        );
        selected.select_thread("new".into(), 2);
        assert!(selected.refresh_ready("new", start));
        assert!(
            selected
                .refresh_deadline(&dirty, &Default::default())
                .is_none()
        );
    }
    #[test]
    fn tool_scope_is_bounded_metadata_only_and_old_or_foreign_registrations_do_not_replace_it() {
        let mut selected = Selection::default();
        selected.select_thread("t".into(), 1);
        assert!(selected.observe_tools("t", 1, scope(5, 3)).unwrap());
        let saved = &selected.tool_scope.as_ref().unwrap().nodes[0];
        assert!(saved.text.is_empty() && saved.presentation.blocks[0].text.is_empty());
        assert_eq!(
            saved.presentation.blocks[0].tool.as_ref().unwrap().call_id,
            "saved-call"
        );
        assert!(!selected.observe_tools("t", 1, scope(4, 3)).unwrap());
        assert!(!selected.observe_tools("t", 1, scope(5, 3)).unwrap());
        assert!(!selected.observe_tools("other", 1, scope(6, 4)).unwrap());
        assert!(!selected.observe_tools("t", 2, scope(6, 4)).unwrap());
        let mut huge = scope(6, 4);
        huge.nodes = vec![huge.nodes[0].clone(); 129];
        assert!(selected.observe_tools("t", 1, huge).is_err());
        assert_eq!(selected.tool_scope.as_ref().unwrap().revision, 5);
        selected.clear_tools("other", 1);
        assert!(selected.tool_scope.is_some());
        selected.clear_tools("t", 1);
        assert!(selected.tool_scope.is_none());
        selected.observe_tools("t", 1, scope(7, 4)).unwrap();
        selected.activate_draft("draft".into());
        assert!(selected.tool_scope.is_none());
    }
}

#[cfg(test)]
mod live_child_tests {
    use super::*;
    use crate::ui::{
        activity::ActivitySnapshot,
        live_work::{LiveWorkRow, LiveWorkSnapshot, LiveWorkTarget},
    };
    fn snapshot() -> ActivitySnapshot {
        ActivitySnapshot {
            live_work: Some(LiveWorkSnapshot {
                run_id: "parent-run".into(),
                rows: vec![LiveWorkRow {
                    target: LiveWorkTarget::ChildRun {
                        run_id: "child-run".into(),
                        thread_id: "child".into(),
                    },
                    label: "child".into(),
                    state: "実行中".into(),
                    started_at: None,
                    elapsed_base: None,
                    last_output_at: None,
                    output_age_base: None,
                }],
                shells: 0,
                children: 1,
                waits: Some(0),
                overflow: 0,
                observed_at: std::time::Instant::now(),
            }),
            ..Default::default()
        }
    }
    #[tokio::test]
    async fn stale_activity_cannot_restore_child_dependencies_after_parent_or_selection_change() {
        let (updates, _receiver) = std::sync::mpsc::sync_channel(16);
        let mut selection = Selection::default();
        selection.select_thread("parent".into(), 2);
        let old_epoch = selection.progress_epoch;
        let project = || activity::Projection {
            snapshot: snapshot(),
            observed_at: std::time::Instant::now(),
            tool_calls: 0,
            cancelled_calls: 0,
            poll_running: false,
        };
        selection
            .publish_activity(&updates, "parent".into(), 2, old_epoch, Ok(project()))
            .await;
        assert!(selection.dependent_parent("child").is_some());
        selection.changed("parent");
        let new_epoch = selection.progress_epoch;
        let mut newer = project();
        newer.snapshot.live_work = None;
        selection
            .publish_activity(&updates, "parent".into(), 2, new_epoch, Ok(newer))
            .await;
        selection
            .publish_activity(&updates, "parent".into(), 2, old_epoch, Ok(project()))
            .await;
        assert!(selection.dependent_parent("child").is_none());
        selection.select_thread("other".into(), 3);
        selection
            .publish_activity(&updates, "parent".into(), 2, new_epoch, Ok(project()))
            .await;
        assert!(selection.dependent_parent("child").is_none());
    }
    #[test]
    fn only_visible_child_dependencies_route_to_parent_and_switch_clears_them() {
        let mut selection = Selection::default();
        selection.select_thread("parent".into(), 2);
        selection.observe_live_children(&snapshot());
        assert_eq!(
            selection.dependent_parent("child").as_deref(),
            Some("parent")
        );
        assert!(selection.dependent_parent("unrelated").is_none());
        let start = tokio::time::Instant::now();
        selection.refresh_started("parent", start);
        let parent = selection.dependent_parent("child").unwrap();
        for _ in 0..100 {
            selection.changed(&parent);
        }
        assert!(!selection.refresh_ready(&parent, start + std::time::Duration::from_millis(499)));
        selection.observe_live_children(&ActivitySnapshot::default());
        assert!(selection.dependent_parent("child").is_none());
        selection.observe_live_children(&snapshot());
        selection.select_thread("other".into(), 3);
        assert!(selection.dependent_parent("child").is_none());
    }
}
