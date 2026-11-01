//! One visible saved-target inspection; no database or command execution in the UI.
use super::*;
#[derive(Clone, Debug, PartialEq)]
pub enum InspectionTarget {
    Lists(ListInspectionScope),
    HistorySources(HistorySourceScope),
    RetainedSources(Box<crate::frontend::inspection_retained::RetainedScope>),
    Operation(String),
    Usage(crate::protocol::UsageGetArgs),
    Thread(String),
    Attachments {
        thread_id: String,
        attachment_id: Option<String>,
    },
    Run(String),
    Turn(String),
    Shell(String),
    Message(String),
    Revisit(String),
    Storage,
    Deletion(ThreadDeleteArgs),
    Index(IndexArgs),
}
#[derive(Clone, Debug, PartialEq)]
pub struct HistorySourceScope {
    pub nodes: Vec<(String, Option<i64>)>,
    pub thread_id: Option<String>,
    pub captured_at: String,
    pub scope_limited: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ListInspectionScope {
    pub(crate) command: crate::protocol::Command,
    pub(crate) original: std::sync::Arc<serde_json::Value>,
    pub(crate) threads: Vec<String>,
    pub(crate) captured_at: String,
}
impl InspectionTarget {
    pub(crate) fn retains_history(&self) -> bool {
        matches!(
            self,
            Self::HistorySources(_) | Self::Lists(_) | Self::RetainedSources(_)
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectionPart {
    Metadata,
    Usage,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectionFieldAnchor {
    pub pointer: String,
    pub entity: Option<(String, String)>,
    pub relative_pointer: String,
    pub stable: bool,
}
impl InspectionFieldAnchor {
    pub(super) fn same_source(&self, other: &Self) -> bool {
        self.stable
            && other.stable
            && match (&self.entity, &other.entity) {
                (Some(left), Some(right)) => {
                    left == right && self.relative_pointer == other.relative_pointer
                }
                (None, None) => self.pointer == other.pointer,
                _ => false,
            }
    }
}
#[derive(Clone, Debug)]
pub struct InspectionSnapshot {
    pub target: InspectionTarget,
    pub metadata_sequence: u64,
    pub usage_sequence: Option<u64>,
    pub metadata_observed_at: String,
    pub usage_observed_at: Option<String>,
    pub fields: Vec<PresentationField>,
    pub anchors: Vec<InspectionFieldAnchor>,
    pub omitted: bool,
}
pub(super) struct State {
    pub retained_fields: Option<Vec<PresentationField>>,
    pub retained_omitted: bool,
    pub target: InspectionTarget,
    pub metadata_sequence: u64,
    pub usage_sequence: Option<u64>,
    pub has_snapshot: bool,
    pub metadata_observed_at: String,
    pub usage_observed_at: Option<String>,
    pub anchors: Vec<InspectionFieldAnchor>,
    pub metadata_error: Option<(u64, String)>,
    pub usage_error: Option<(u64, String)>,
    pub selection_unavailable: bool,
    pub reset_selection: bool,
    pub unavailable_selection: Option<InspectionFieldAnchor>,
    pub detail_anchor: Option<InspectionFieldAnchor>,
    pub detail_requested: bool,
    pub detail_pointer: Option<String>,
}
impl State {
    pub fn new(target: InspectionTarget) -> Self {
        Self {
            retained_fields: None,
            retained_omitted: false,
            target,
            metadata_sequence: 0,
            usage_sequence: None,
            has_snapshot: false,
            metadata_observed_at: String::new(),
            usage_observed_at: None,
            anchors: vec![],
            metadata_error: None,
            usage_error: None,
            selection_unavailable: false,
            reset_selection: false,
            unavailable_selection: None,
            detail_anchor: None,
            detail_requested: false,
            detail_pointer: None,
        }
    }
    pub fn label(&self) -> String {
        let mut text = if self.has_snapshot {
            format!("状態取得 {}", self.metadata_observed_at)
        } else {
            "保存状態を取得中".into()
        };
        if let InspectionTarget::RetainedSources(scope) = &self.target {
            text.push_str(&format!(
                " | 取得済み結果 {} を保持 | 再実行は a から明示操作",
                scope.captured_at
            ));
        }
        if let InspectionTarget::HistorySources(scope) = &self.target {
            text.push_str(&format!(
                " | 取得時の結果 {} を保持 | 再取得:aから同じ履歴照会",
                scope.captured_at
            ));
        }
        if let InspectionTarget::Lists(scope) = &self.target {
            text.push_str(&format!(
                " | 取得時の一覧 {} を保持 | 新しい行はaから再取得",
                scope.captured_at
            ));
        }
        if let Some(at) = &self.usage_observed_at {
            text.push_str(&format!(" | 使用量取得 {at}"));
        }
        for (kind, error) in [
            ("状態", &self.metadata_error),
            ("使用量", &self.usage_error),
        ] {
            if let Some((_, error)) = error {
                text.push_str(&format!(" | {kind}未更新: {error}"));
            }
        }
        if self.selection_unavailable {
            if matches!(self.target, InspectionTarget::Attachments { .. }) {
                text.push_str(
                    " | 選択した添付は今回の取得範囲で未確認・上下キーで選び直してください",
                );
            } else {
                text.push_str(" | 選択元は利用不可・上下キーで選び直してください");
            }
        }
        text
    }
}
impl UiState {
    pub(super) fn stamp_inspection_action(&mut self, action: &mut UiAction) {
        match action {
            UiAction::ObserveInspection { revision, .. } if *revision == 0 => {
                self.inspection_revision = self.inspection_revision.wrapping_add(1).max(1);
                *revision = self.inspection_revision;
            }
            UiAction::ManagementIntent { action, .. }
            | UiAction::SubmittedCommand { action, .. } => self.stamp_inspection_action(action),
            _ => {}
        }
    }
    pub(super) fn observe_inspection(&mut self) {
        let desired = self
            .management
            .inspection
            .as_ref()
            .and_then(|inspection| {
                self.management
                    .owner
                    .clone()
                    .map(|context| (context, inspection.target.clone()))
            })
            .filter(|_| {
                self.top_overlay() == Some(overlay::Overlay::Management)
                    && !self.selection_mode
                    && !self.is_quitting()
            });
        if self.inspection_observation.as_ref() == desired.as_ref() {
            return;
        }
        if let Some((context, target)) = self.inspection_observation.take() {
            self.deferred_actions.retain(|action|!matches!(action,UiAction::ObserveInspection {context:c,target:t,..} if c==&context && t==&target));
            let mut action = UiAction::ObserveInspection {
                context,
                target,
                visible: false,
                revision: 0,
            };
            self.stamp_inspection_action(&mut action);
            self.deferred_actions.push(action);
        }
        if let Some((context, target)) = desired {
            self.deferred_actions.retain(|action|!matches!(action,UiAction::ObserveInspection {context:c,target:t,..} if c==&context && t==&target));
            let mut action = UiAction::ObserveInspection {
                context: context.clone(),
                target: target.clone(),
                visible: true,
                revision: 0,
            };
            self.stamp_inspection_action(&mut action);
            self.deferred_actions.push(action);
            self.inspection_observation = Some((context, target));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (UiState, ManagementContext) {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        let context = ManagementContext {
            epoch: 7,
            owner_draft: ui.view().draft.draft_id.clone(),
            owner_thread: None,
        };
        ui.management.epoch = 7;
        ui.management.owner = Some(context.clone());
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionStarted {
                target: InspectionTarget::Run("r".into()),
                title: "saved Run".into(),
            },
        );
        (ui, context)
    }
    fn scoped(ui: &mut UiState, context: &ManagementContext, update: UiUpdate) {
        ui.apply(UiUpdate::ScopedManagement {
            context: context.clone(),
            update: Box::new(update),
        });
    }
    fn snapshot(sequence: u64, usage: Option<u64>, order: &[&str]) -> InspectionSnapshot {
        InspectionSnapshot {
            target: InspectionTarget::Run("r".into()),
            metadata_sequence: sequence,
            usage_sequence: usage,
            metadata_observed_at: format!("read-{sequence}"),
            usage_observed_at: usage.map(|n| format!("usage-{n}")),
            omitted: false,
            fields: order
                .iter()
                .enumerate()
                .map(|(index, id)| PresentationField {
                    key: format!("/items/{index}/state"),
                    label: format!("{id} state"),
                    value: format!("saved {id}"),
                    ..Default::default()
                })
                .collect(),
            anchors: order
                .iter()
                .enumerate()
                .map(|(index, id)| InspectionFieldAnchor {
                    pointer: format!("/items/{index}/state"),
                    entity: Some(("run".into(), (*id).into())),
                    relative_pointer: "/state".into(),
                    stable: true,
                })
                .collect(),
        }
    }
    fn detail(action: &UiAction) -> Option<(u64, u64, String, Option<String>)> {
        match action {
            UiAction::ManagementIntent { action, .. } => detail(action),
            UiAction::LoadManagementDetail {
                request_id,
                detail_request_id,
                pointer,
                cursor,
            } => Some((
                *request_id,
                *detail_request_id,
                pointer.clone(),
                cursor.clone(),
            )),
            _ => None,
        }
    }
    #[test]
    fn reactive_snapshot_remaps_entity_then_refreshes_exact_detail_without_discarding_old_text_or_scroll()
     {
        let (mut ui, context) = setup();
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 1,
                snapshot: snapshot(10, Some(90), &["a", "b"]),
            },
        );
        ui.management.structured_mut().unwrap().selected = 1;
        let actions = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let (request, token, pointer, _) = actions.iter().find_map(detail).unwrap();
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: request,
                detail_request_id: token,
                pointer,
                text: "PAID OLD DETAIL".into(),
                next_cursor: Some("old-cursor".into()),
            },
        );
        ui.management.view_mut().unwrap().scroll = 12;
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 2,
                snapshot: snapshot(11, Some(90), &["b", "a"]),
            },
        );
        assert_eq!(ui.management.structured().unwrap().selected, 0);
        let old = ui.management.structured().unwrap().detail.as_ref().unwrap();
        assert_eq!(old.text, "PAID OLD DETAIL");
        assert_eq!(old.source_request_id, 1);
        assert!(old.stale.is_some());
        assert_eq!(ui.management.as_ref().unwrap().scroll, 12);
        let actions = ui.take_deferred_actions();
        let (request, new_token, pointer, cursor) = actions.iter().find_map(detail).unwrap();
        assert_eq!(request, 2);
        assert_eq!(pointer, "/items/0/state");
        assert_eq!(cursor, None);
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: 1,
                detail_request_id: token,
                pointer: "/items/1/state".into(),
                text: "LATE OLD PAGE".into(),
                next_cursor: None,
            },
        );
        assert_eq!(
            ui.management
                .structured()
                .unwrap()
                .detail
                .as_ref()
                .unwrap()
                .text,
            "PAID OLD DETAIL"
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: request,
                detail_request_id: new_token,
                pointer,
                text: "FRESH SAME ENTITY".into(),
                next_cursor: None,
            },
        );
        assert_eq!(
            ui.management
                .structured()
                .unwrap()
                .detail
                .as_ref()
                .unwrap()
                .text,
            "FRESH SAME ENTITY"
        );
        assert_eq!(ui.management.as_ref().unwrap().scroll, 12);
    }
    #[test]
    fn usage_failure_floor_does_not_reject_cached_success_when_metadata_advances() {
        let (mut ui, context) = setup();
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 1,
                snapshot: snapshot(10, Some(90), &["a"]),
            },
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionFailure {
                target: InspectionTarget::Run("r".into()),
                part: InspectionPart::Usage,
                read_sequence: 100,
                observed_at: "failed-at".into(),
                error: "unavailable".into(),
            },
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 2,
                snapshot: snapshot(110, Some(90), &["a"]),
            },
        );
        let state = ui.management.inspection.as_ref().unwrap();
        assert_eq!(state.metadata_sequence, 110);
        assert_eq!(state.usage_sequence, Some(90));
        assert!(state.usage_error.is_some());
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 3,
                snapshot: snapshot(110, Some(101), &["a"]),
            },
        );
        assert!(
            ui.management
                .inspection
                .as_ref()
                .unwrap()
                .usage_error
                .is_none()
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 99,
                snapshot: snapshot(109, Some(200), &["wrong"]),
            },
        );
        assert_eq!(ui.management.structured().unwrap().request_id, 3);
    }
    #[test]
    fn entity_disappearance_and_unstable_array_anchors_never_retarget_enter() {
        for stable in [true, false] {
            let (mut ui, context) = setup();
            let mut initial = snapshot(10, None, &["a"]);
            initial.anchors[0].stable = stable;
            scoped(
                &mut ui,
                &context,
                UiUpdate::InspectionSnapshot {
                    request_id: 1,
                    snapshot: initial,
                },
            );
            let next = if stable {
                snapshot(11, None, &["other"])
            } else {
                let mut next = snapshot(11, None, &["a"]);
                next.anchors[0].stable = false;
                next
            };
            scoped(
                &mut ui,
                &context,
                UiUpdate::InspectionSnapshot {
                    request_id: 2,
                    snapshot: next,
                },
            );
            assert!(
                ui.management
                    .inspection
                    .as_ref()
                    .unwrap()
                    .selection_unavailable
            );
            assert!(
                ui.event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                )))
                .iter()
                .all(|action| detail(action).is_none())
            );
        }
    }
    #[test]
    fn top_only_scope_clears_on_cover_and_resumes_once_while_queued_snapshot_is_ignored() {
        let (mut ui, context) = setup();
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 1,
                snapshot: snapshot(10, None, &["a"]),
            },
        );
        let scope = ui.take_deferred_actions();
        assert_eq!(
            scope
                .iter()
                .filter(|action| matches!(
                    action,
                    UiAction::ObserveInspection { visible: true, .. }
                ))
                .count(),
            1
        );
        assert!(
            ui.take_deferred_actions()
                .iter()
                .all(|action| !matches!(action, UiAction::ObserveInspection { .. }))
        );
        ui.details = Some(details::Details::new(ui.view(), false));
        assert!(
            ui.take_deferred_actions()
                .iter()
                .any(|action| matches!(action, UiAction::ObserveInspection { visible: false, .. }))
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 2,
                snapshot: snapshot(11, None, &["covered"]),
            },
        );
        assert_eq!(ui.management.structured().unwrap().request_id, 1);
        ui.details = None;
        assert!(
            ui.take_deferred_actions()
                .iter()
                .any(|action| matches!(action, UiAction::ObserveInspection { visible: true, .. }))
        );
        ui.selection_mode = true;
        assert!(
            ui.take_deferred_actions()
                .iter()
                .any(|action| matches!(action, UiAction::ObserveInspection { visible: false, .. }))
        );
        ui.selection_mode = false;
        ui.take_deferred_actions();
        ui.management.close();
        assert!(
            ui.take_deferred_actions()
                .iter()
                .any(|action| matches!(action, UiAction::ObserveInspection { visible: false, .. }))
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 3,
                snapshot: snapshot(12, None, &["late"]),
            },
        );
        assert!(ui.management.is_none());
    }
    #[test]
    fn detail_page_tokens_reject_late_first_or_next_and_failure_keeps_paid_copy() {
        let (mut ui, context) = setup();
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 1,
                snapshot: snapshot(10, None, &["a"]),
            },
        );
        let first = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let (_, token, pointer, _) = first.iter().find_map(detail).unwrap();
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: 1,
                detail_request_id: token,
                pointer: pointer.clone(),
                text: "PAID".into(),
                next_cursor: Some("next".into()),
            },
        );
        let next = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        )));
        let (_, next_token, _, cursor) = next.iter().find_map(detail).unwrap();
        assert_eq!(cursor.as_deref(), Some("next"));
        let reset = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::NONE,
        )));
        let (_, reset_token, _, cursor) = reset.iter().find_map(detail).unwrap();
        assert_eq!(cursor, None);
        assert_ne!(reset_token, next_token);
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: 1,
                detail_request_id: next_token,
                pointer: pointer.clone(),
                text: "LATE NEXT".into(),
                next_cursor: None,
            },
        );
        assert_eq!(
            ui.management
                .structured()
                .unwrap()
                .detail
                .as_ref()
                .unwrap()
                .text,
            "PAID"
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetailFailure {
                request_id: 1,
                detail_request_id: reset_token,
                pointer,
                error: "source unavailable".into(),
            },
        );
        let old = ui.management.structured().unwrap().detail.as_ref().unwrap();
        assert_eq!(old.text, "PAID");
        assert!(old.stale.as_ref().unwrap().contains("source unavailable"));
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 2,
                snapshot: snapshot(11, None, &["replacement"]),
            },
        );
        let copy = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE,
        )));
        assert!(
            copy.iter()
                .any(|action| matches!(action,UiAction::CopyText {text,..} if text=="PAID")),
            "unavailable live source does not hide already obtained text"
        );
        assert!(
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::Char('n'),
                KeyModifiers::NONE
            )))
            .iter()
            .all(|action| detail(action).is_none()),
            "old cursor cannot be used against replacement Value"
        );
    }
    #[test]
    fn missing_selection_keeps_original_anchor_across_repeated_patches_until_deliberate_choice() {
        let (mut ui, context) = setup();
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 1,
                snapshot: snapshot(10, None, &["a"]),
            },
        );
        for (request_id, sequence) in [(2, 11), (3, 12)] {
            scoped(
                &mut ui,
                &context,
                UiUpdate::InspectionSnapshot {
                    request_id,
                    snapshot: snapshot(sequence, None, &["b"]),
                },
            );
            let state = ui.management.inspection.as_ref().unwrap();
            assert!(state.selection_unavailable);
            assert_eq!(
                state
                    .unavailable_selection
                    .as_ref()
                    .unwrap()
                    .entity
                    .as_ref()
                    .unwrap()
                    .1,
                "a"
            );
            assert!(
                ui.event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                )))
                .iter()
                .all(|action| detail(action).is_none())
            );
        }
        ui.event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert!(
            !ui.management
                .inspection
                .as_ref()
                .unwrap()
                .selection_unavailable
        );
        let actions = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(
            actions.iter().any(|action| detail(action).is_some()),
            "explicit selection can choose current b"
        );
    }
    #[test]
    fn copying_stale_page_never_reanchors_its_automatic_refresh_to_fallback_entity() {
        let (mut ui, context) = setup();
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 1,
                snapshot: snapshot(10, None, &["a"]),
            },
        );
        let first = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let (_, token, pointer, _) = first.iter().find_map(detail).unwrap();
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: 1,
                detail_request_id: token,
                pointer,
                text: "PAID A".into(),
                next_cursor: Some("old".into()),
            },
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 2,
                snapshot: snapshot(11, None, &["b"]),
            },
        );
        let copy = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE,
        )));
        assert!(
            copy.iter()
                .any(|action| matches!(action,UiAction::CopyText {text,..} if text=="PAID A"))
        );
        assert_eq!(
            ui.management
                .inspection
                .as_ref()
                .unwrap()
                .detail_anchor
                .as_ref()
                .unwrap()
                .entity
                .as_ref()
                .unwrap()
                .1,
            "a"
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 3,
                snapshot: snapshot(12, None, &["b"]),
            },
        );
        assert!(
            ui.take_deferred_actions()
                .iter()
                .all(|action| detail(action).is_none()),
            "copy must not initiate b's detail on later refresh"
        );
        assert_eq!(
            ui.management
                .structured()
                .unwrap()
                .detail
                .as_ref()
                .unwrap()
                .text,
            "PAID A"
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 4,
                snapshot: snapshot(13, None, &["b", "a"]),
            },
        );
        let actions = ui.take_deferred_actions();
        let (request, _, pointer, cursor) = actions.iter().find_map(detail).unwrap();
        assert_eq!(request, 4);
        assert_eq!(pointer, "/items/1/state");
        assert_eq!(cursor, None);
    }
    #[test]
    fn parent_refresh_reissues_pending_first_detail_and_rejects_superseded_response() {
        let (mut ui, context) = setup();
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 1,
                snapshot: snapshot(10, None, &["a", "b"]),
            },
        );
        let first = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let (_, old_token, old_pointer, _) = first.iter().find_map(detail).unwrap();
        assert!(ui.management.structured().unwrap().detail.is_none());
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 2,
                snapshot: snapshot(11, None, &["b", "a"]),
            },
        );
        let actions = ui.take_deferred_actions();
        let (request, new_token, pointer, cursor) = actions
            .iter()
            .find_map(detail)
            .expect("pending first page is still a live consumer");
        assert_eq!(request, 2);
        assert_ne!(new_token, old_token);
        assert_eq!(pointer, "/items/1/state");
        assert_eq!(cursor, None);
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: 1,
                detail_request_id: old_token,
                pointer: old_pointer,
                text: "OLD FIRST".into(),
                next_cursor: None,
            },
        );
        assert!(ui.management.structured().unwrap().detail.is_none());
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: request,
                detail_request_id: new_token,
                pointer,
                text: "CURRENT A".into(),
                next_cursor: None,
            },
        );
        assert_eq!(
            ui.management
                .structured()
                .unwrap()
                .detail
                .as_ref()
                .unwrap()
                .text,
            "CURRENT A"
        );
        ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 3,
                snapshot: snapshot(12, None, &["b", "a"]),
            },
        );
        assert!(
            ui.take_deferred_actions()
                .iter()
                .all(|action| detail(action).is_none()),
            "dismissed detail is no longer a consumer"
        );
    }
    fn index_snapshot(sequence: u64, args: IndexArgs, next: bool) -> InspectionSnapshot {
        let pointer = "/management/coverage_window/next_cursor";
        InspectionSnapshot {
            target: InspectionTarget::Index(args),
            metadata_sequence: sequence,
            usage_sequence: None,
            metadata_observed_at: format!("index-{sequence}"),
            usage_observed_at: None,
            fields: vec![PresentationField {
                key: pointer.into(),
                label: "next cursor".into(),
                value: if next {
                    "opaque-current-window"
                } else {
                    "null"
                }
                .into(),
                ..Default::default()
            }],
            anchors: vec![InspectionFieldAnchor {
                pointer: pointer.into(),
                entity: None,
                relative_pointer: pointer.into(),
                stable: true,
            }],
            omitted: false,
        }
    }
    #[test]
    fn index_inspection_preserves_cost_actions_and_ctrl_r_does_not_replace_plain_rebuild() {
        let (mut ui, context) = setup();
        let args = IndexArgs {
            scope: Some(IndexScope::All),
            ..Default::default()
        };
        ui.management.scope = Some(args.clone());
        ui.management.mode = management::ManagementMode::Index;
        ui.management.preview =
            management::IndexPreviewState::Ready(args.clone(), "saved cost preview".into());
        ui.management.ready_preview_attempt = Some(1);
        ui.management.pending = Some(Command::IndexRetry(args.clone()));
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionStarted {
                target: InspectionTarget::Index(args.clone()),
                title: "saved index".into(),
            },
        );
        assert_eq!(ui.management.mode, management::ManagementMode::Index);
        assert_eq!(ui.management.scope, Some(args.clone()));
        assert!(
            matches!(&ui.management.preview,management::IndexPreviewState::Ready(saved,text) if saved==&args && text=="saved cost preview")
        );
        assert!(matches!(&ui.management.pending,Some(Command::IndexRetry(saved)) if saved==&args));
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 20,
                snapshot: index_snapshot(10, args.clone(), false),
            },
        );
        let refresh = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        )));
        assert!(
            matches!(refresh.as_slice(),[UiAction::ObserveInspection {context:c,target:InspectionTarget::Index(saved),visible:true,..}] if c==&context && saved==&args)
        );
        assert!(ui.confirmation.is_none());
        let rebuild = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        )));
        assert!(rebuild.is_empty());
        assert!(
            matches!(&ui.confirmation,Some(Confirmation::Index {command:Command::IndexRebuild(saved),..}) if saved==&args)
        );
    }
    #[test]
    fn index_next_window_uses_current_scope_and_request_only_when_next_fact_exists() {
        let (mut ui, context) = setup();
        let args = IndexArgs {
            thread_ids: vec!["target".into()],
            ..Default::default()
        };
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionStarted {
                target: InspectionTarget::Index(args.clone()),
                title: "saved index".into(),
            },
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 10,
                snapshot: index_snapshot(1, args.clone(), false),
            },
        );
        assert!(
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::Char('n'),
                KeyModifiers::NONE
            )))
            .is_empty()
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 11,
                snapshot: index_snapshot(2, args.clone(), true),
            },
        );
        let next = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        )));
        assert!(
            matches!(next.as_slice(),[UiAction::NextInspectionWindow {context:c,request_id:11,target:InspectionTarget::Index(saved)}] if c==&context && saved==&args)
        );
        assert!(ui.management.inspection.as_ref().unwrap().reset_selection);
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 12,
                snapshot: index_snapshot(3, args.clone(), true),
            },
        );
        let actions = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let (_, token, pointer, _) = actions.iter().find_map(detail).unwrap();
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: 12,
                detail_request_id: token,
                pointer,
                text: "current field".into(),
                next_cursor: Some("detail-next".into()),
            },
        );
        let next_detail = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        )));
        assert!(
            next_detail
                .iter()
                .all(|action| !matches!(action, UiAction::NextInspectionWindow { .. }))
        );
        assert!(next_detail.iter().any(|action| {
            detail(action).is_some_and(|(_, _, _, cursor)| cursor.as_deref() == Some("detail-next"))
        }));
    }
    #[test]
    fn parent_render_fence_preserves_new_physical_facts_with_equal_metadata_watermark() {
        let (mut ui, context) = setup();
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionStarted {
                target: InspectionTarget::Storage,
                title: "saved storage".into(),
            },
        );
        let storage = |value: &str| InspectionSnapshot {
            target: InspectionTarget::Storage,
            metadata_sequence: 10,
            usage_sequence: None,
            metadata_observed_at: "same DB snapshot".into(),
            usage_observed_at: None,
            fields: vec![PresentationField {
                key: "/physical/sequence".into(),
                label: "physical sequence".into(),
                value: value.into(),
                ..Default::default()
            }],
            anchors: vec![],
            omitted: false,
        };
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 3,
                snapshot: storage("new physical sample"),
            },
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 2,
                snapshot: storage("old physical sample"),
            },
        );
        assert_eq!(ui.management.structured().unwrap().request_id, 3);
        assert_eq!(
            ui.management.structured().unwrap().fields[0].value,
            "new physical sample"
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 4,
                snapshot: storage("next physical sample"),
            },
        );
        assert_eq!(
            ui.management.structured().unwrap().fields[0].value,
            "next physical sample"
        );
        assert!(
            ui.management
                .inspection
                .as_ref()
                .unwrap()
                .usage_observed_at
                .is_none()
        );
    }

    #[test]
    fn index_scope_change_cannot_pair_new_cost_preview_with_old_pending_command() {
        let (mut ui, context) = setup();
        let a = IndexArgs {
            thread_ids: vec!["scope-a".into()],
            ..Default::default()
        };
        let b = IndexArgs {
            thread_ids: vec!["scope-b".into()],
            ..Default::default()
        };
        ui.management.mode = management::ManagementMode::Index;
        ui.management.scope = Some(a.clone());
        ui.management.pending = Some(Command::IndexRebuild(a.clone()));
        let attempt = ui.management.begin_preview(a.clone());
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionStarted {
                target: InspectionTarget::Index(b.clone()),
                title: "scope b".into(),
            },
        );
        assert!(ui.management.pending.is_none());
        assert!(matches!(
            ui.management.preview,
            management::IndexPreviewState::Idle
        ));
        assert_eq!(ui.management.scope, Some(b.clone()));
        scoped(
            &mut ui,
            &context,
            UiUpdate::IndexPreviewReady {
                args: b,
                attempt,
                text: "cost for b".into(),
            },
        );
        assert!(
            ui.confirmation.is_none(),
            "scope-b preview must never confirm scope-a mutation"
        );
        ui.management.pending = Some(Command::IndexRetry(a.clone()));
        ui.management.preview = management::IndexPreviewState::Loading(a.clone());
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionStarted {
                target: InspectionTarget::Run("another-run".into()),
                title: "saved run".into(),
            },
        );
        assert!(ui.management.pending.is_none());
        assert!(matches!(
            ui.management.preview,
            management::IndexPreviewState::Idle
        ));
        assert!(ui.management.scope.is_none());
        scoped(
            &mut ui,
            &context,
            UiUpdate::IndexPreviewReady {
                args: a,
                attempt,
                text: "late old cost".into(),
            },
        );
        assert!(ui.confirmation.is_none());
    }
    #[test]
    fn attachment_reference_selects_exact_identity_or_explicit_unknown_without_retargeting() {
        let (mut ui, context) = setup();
        let target = InspectionTarget::Attachments {
            thread_id: "foreign".into(),
            attachment_id: Some("wanted".into()),
        };
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionStarted {
                target: target.clone(),
                title: "saved refs".into(),
            },
        );
        let make = |sequence, ids: &[&str]| InspectionSnapshot {
            target: target.clone(),
            metadata_sequence: sequence,
            usage_sequence: None,
            metadata_observed_at: "now".into(),
            usage_observed_at: None,
            omitted: false,
            fields: ids
                .iter()
                .enumerate()
                .map(|(n, id)| PresentationField {
                    key: format!("/attachments/{n}/attachment_id"),
                    label: "attachment".into(),
                    value: (*id).into(),
                    ..Default::default()
                })
                .collect(),
            anchors: ids
                .iter()
                .enumerate()
                .map(|(n, id)| InspectionFieldAnchor {
                    pointer: format!("/attachments/{n}/attachment_id"),
                    entity: Some(("attachment_selection".into(), format!("foreign/{id}"))),
                    relative_pointer: "/attachment_id".into(),
                    stable: true,
                })
                .collect(),
        };
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 1,
                snapshot: make(1, &["other"]),
            },
        );
        assert!(
            ui.management
                .inspection
                .as_ref()
                .unwrap()
                .selection_unavailable
        );
        assert!(
            ui.management
                .inspection
                .as_ref()
                .unwrap()
                .label()
                .contains("未確認")
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 2,
                snapshot: make(2, &["other", "wanted"]),
            },
        );
        assert_eq!(ui.management.structured().unwrap().selected, 1);
        assert!(
            !ui.management
                .inspection
                .as_ref()
                .unwrap()
                .selection_unavailable
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 3,
                snapshot: make(3, &["wanted", "other"]),
            },
        );
        assert_eq!(ui.management.structured().unwrap().selected, 0);
    }
    #[test]
    fn limited_operation_outcome_preserves_open_paid_detail_source_and_rejects_late_page() {
        let (mut ui, context) = setup();
        let target = InspectionTarget::Operation("old:exact".into());
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionStarted {
                target: target.clone(),
                title: "saved operation".into(),
            },
        );
        let snap = |sequence, value: &serde_json::Value| {
            let fields = crate::presentation::fields_of_value(value);
            let anchors = fields
                .iter()
                .map(|f| InspectionFieldAnchor {
                    pointer: f.key.clone(),
                    entity: None,
                    relative_pointer: f.key.clone(),
                    stable: true,
                })
                .collect();
            InspectionSnapshot {
                target: target.clone(),
                metadata_sequence: sequence,
                usage_sequence: None,
                metadata_observed_at: format!("read-{sequence}"),
                usage_observed_at: None,
                fields,
                anchors,
                omitted: false,
            }
        };
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 1,
                snapshot: snap(
                    10,
                    &serde_json::json!({"outcome":{"result":{"text":"paid"}}}),
                ),
            },
        );
        let actions = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let (request, token, pointer, _) = actions.iter().find_map(detail).unwrap();
        assert_eq!(pointer, "/outcome/result/text");
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: request,
                detail_request_id: token,
                pointer: pointer.clone(),
                text: "PAID OPERATION TEXT".into(),
                next_cursor: Some("old-page".into()),
            },
        );
        ui.management.view_mut().unwrap().scroll = 4;
        scoped(
            &mut ui,
            &context,
            UiUpdate::InspectionSnapshot {
                request_id: 2,
                snapshot: snap(
                    11,
                    &serde_json::json!({"outcome":{"observation":"unavailable","reason":"display limit"},"targets":null}),
                ),
            },
        );
        let page = ui.management.structured().unwrap().detail.as_ref().unwrap();
        assert_eq!(page.text, "PAID OPERATION TEXT");
        assert_eq!(page.source_request_id, 1);
        assert!(page.stale.is_some());
        assert_eq!(ui.management.as_ref().unwrap().scroll, 4);
        assert!(
            ui.take_deferred_actions()
                .iter()
                .all(|action| detail(action).is_none()),
            "missing old source cannot silently fetch a replacement field"
        );
        scoped(
            &mut ui,
            &context,
            UiUpdate::ManagementDetail {
                request_id: request,
                detail_request_id: token,
                pointer,
                text: "LATE WRONG PAGE".into(),
                next_cursor: None,
            },
        );
        assert_eq!(
            ui.management
                .structured()
                .unwrap()
                .detail
                .as_ref()
                .unwrap()
                .text,
            "PAID OPERATION TEXT"
        );
    }
    #[test]
    fn retained_history_keeps_original_fields_and_exact_selection_while_current_facts_change() {
        let (mut ui, context) = setup();
        ui.management.open_structured(
            "History".into(),
            40,
            vec![PresentationField {
                key: "/text".into(),
                label: "本文".into(),
                value: "retained original".into(),
                ..Default::default()
            }],
            false,
            false,
        );
        let target = InspectionTarget::HistorySources(HistorySourceScope {
            nodes: vec![("n".into(), Some(7))],
            thread_id: None,
            captured_at: "obtained-at".into(),
            scope_limited: false,
        });
        assert!(!ui.management.start_retained_inspection(39, target.clone()));
        assert!(ui.management.start_retained_inspection(40, target.clone()));
        ui.management.inspection.as_mut().unwrap().detail_anchor = Some(InspectionFieldAnchor {
            pointer: "/text".into(),
            entity: None,
            relative_pointer: "/text".into(),
            stable: true,
        });
        ui.management.structured_mut().unwrap().detail = Some(management::ManagementPage {
            source_request_id: 40,
            observed_at: "obtained-at".into(),
            stale: None,
            pointer: "/text".into(),
            text: "retained original full page".into(),
            next_cursor: Some("retained cursor".into()),
        });

        for (sequence, available) in [(10, "true"), (11, "false")] {
            let snap = InspectionSnapshot {
                target: target.clone(),
                metadata_sequence: sequence,
                usage_sequence: None,
                metadata_observed_at: format!("current-{sequence}"),
                usage_observed_at: None,
                fields: vec![PresentationField {
                    key: "/current_source/nodes/0/body_available".into(),
                    label: "現在 | 原文".into(),
                    value: available.into(),
                    ..Default::default()
                }],
                anchors: vec![InspectionFieldAnchor {
                    pointer: "/current_source/nodes/0/body_available".into(),
                    entity: Some(("node".into(), "n".into())),
                    relative_pointer: "/body_available".into(),
                    stable: true,
                }],
                omitted: false,
            };
            assert!(ui.management.patch_inspection(sequence + 40, snap));
            let state = ui.management.structured().unwrap();
            assert_eq!(state.fields[state.selected].key, "/text");
            let page = state.detail.as_ref().unwrap();
            assert_eq!(page.source_request_id, sequence + 40);
            assert_eq!(page.text, "retained original full page");
            assert_eq!(page.observed_at, "obtained-at");
            assert_eq!(page.next_cursor.as_deref(), Some("retained cursor"));
            assert!(page.stale.is_none() && state.pending_pointer.is_none());

            assert_eq!(
                state
                    .fields
                    .iter()
                    .find(|f| f.key == "/text")
                    .unwrap()
                    .value,
                "retained original"
            );
            assert_eq!(
                state
                    .fields
                    .iter()
                    .find(|f| f.key == "/current_source/nodes/0/body_available")
                    .unwrap()
                    .value,
                available
            );
            assert!(
                ui.management
                    .inspection
                    .as_ref()
                    .unwrap()
                    .label()
                    .contains("obtained-at")
            );
        }
        let _ = context;
    }
    #[test]
    fn retained_original_pending_next_and_first_page_survive_metadata_request_fence() {
        for (key, cursor, replacement) in [
            ('n', Some("next-page-cursor"), "next original page"),
            ('p', None, "first original page"),
        ] {
            let (mut ui, _) = setup();
            ui.management.open_structured(
                "History".into(),
                40,
                vec![PresentationField {
                    key: "/text".into(),
                    label: "本文".into(),
                    value: "original preview".into(),
                    ..Default::default()
                }],
                false,
                false,
            );
            let target = InspectionTarget::HistorySources(HistorySourceScope {
                nodes: vec![("n".into(), Some(7))],
                thread_id: None,
                captured_at: "obtained-at".into(),
                scope_limited: false,
            });
            assert!(ui.management.start_retained_inspection(40, target.clone()));
            let inspection = ui.management.inspection.as_mut().unwrap();
            inspection.detail_anchor = Some(InspectionFieldAnchor {
                pointer: "/text".into(),
                entity: None,
                relative_pointer: "/text".into(),
                stable: true,
            });
            inspection.detail_pointer = Some("/text".into());
            ui.management.structured_mut().unwrap().detail = Some(management::ManagementPage {
                source_request_id: 40,
                observed_at: "obtained-at".into(),
                stale: None,
                pointer: "/text".into(),
                text: "visible original page".into(),
                next_cursor: Some("next-page-cursor".into()),
            });
            let management::Outcome::Actions(actions) = ui.management.key(
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                10,
                None,
            ) else {
                panic!("original page key must request a page");
            };
            let first = actions
                .iter()
                .find_map(detail)
                .expect("initial page intent");
            assert_eq!(first, (40, 1, "/text".into(), cursor.map(str::to_owned)));
            assert_eq!(
                ui.management
                    .structured()
                    .unwrap()
                    .pending_cursor
                    .as_deref(),
                cursor
            );
            let current = InspectionSnapshot {
                target,
                metadata_sequence: 10,
                usage_sequence: None,
                metadata_observed_at: "current-at".into(),
                usage_observed_at: None,
                fields: vec![PresentationField {
                    key: "/current_source/nodes/0/body_available".into(),
                    label: "現在の原文".into(),
                    value: "false".into(),
                    ..Default::default()
                }],
                anchors: vec![InspectionFieldAnchor {
                    pointer: "/current_source/nodes/0/body_available".into(),
                    entity: Some(("node".into(), "n".into())),
                    relative_pointer: "/body_available".into(),
                    stable: true,
                }],
                omitted: false,
            };
            assert!(ui.management.patch_inspection(41, current));
            let reissued = ui
                .management
                .take_inspection_detail()
                .expect("pending original page must be reissued");
            assert_eq!(
                detail(&reissued),
                Some((41, 2, "/text".into(), cursor.map(str::to_owned)))
            );
            {
                let state = ui.management.structured().unwrap();
                let page = state.detail.as_ref().unwrap();
                assert_eq!(page.text, "visible original page");
                assert_eq!(page.source_request_id, 41);
                assert_eq!(page.observed_at, "obtained-at");
                assert_eq!(page.next_cursor.as_deref(), Some("next-page-cursor"));
                assert!(page.stale.is_none());
                assert_eq!(state.pending_pointer.as_deref(), Some("/text"));
                assert_eq!(state.pending_cursor.as_deref(), cursor);
                assert!(
                    !state.preserve_detail_scroll,
                    "retain the requested page's scroll intent"
                );
            }
            ui.management
                .accept_detail(40, 1, "/text".into(), "late old page".into(), None);
            assert_eq!(
                ui.management
                    .structured()
                    .unwrap()
                    .detail
                    .as_ref()
                    .unwrap()
                    .text,
                "visible original page"
            );
            assert_eq!(
                ui.management
                    .structured()
                    .unwrap()
                    .pending_pointer
                    .as_deref(),
                Some("/text")
            );
            ui.management.accept_detail(
                41,
                2,
                "/text".into(),
                replacement.into(),
                Some("following-page".into()),
            );
            let state = ui.management.structured().unwrap();
            let page = state.detail.as_ref().unwrap();
            assert_eq!(page.text, replacement);
            assert_eq!(page.observed_at, "obtained-at");
            assert_eq!(page.next_cursor.as_deref(), Some("following-page"));
            assert!(state.pending_pointer.is_none() && state.pending_cursor.is_none());
            ui.management
                .accept_detail(40, 1, "/text".into(), "late old page".into(), None);
            assert_eq!(
                ui.management
                    .structured()
                    .unwrap()
                    .detail
                    .as_ref()
                    .unwrap()
                    .text,
                replacement
            );
        }
    }
}
