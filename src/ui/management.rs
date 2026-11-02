//! UI-local management content, request lifetime, index selection and drawing.
use super::i18n::local;
use super::*;
use crate::config::UiLanguage;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ManagementMode {
    General,
    Received,
    Status,
    Index,
    Attachments,
}
pub(super) struct ManagementPage {
    pub(super) source_request_id: u64,
    pub(super) observed_at: String,
    pub(super) stale: Option<String>,
    pub(super) pointer: String,
    pub(super) text: String,
    pub(super) next_cursor: Option<String>,
}
pub(super) struct StructuredManagementState {
    pub(super) detail_request_id: u64,
    pub(super) preserve_detail_scroll: bool,
    pub(super) detail_error: Option<String>,
    pub(super) request_id: u64,
    pub(super) fields: Vec<PresentationField>,
    pub(super) selected: usize,
    pub(super) omitted: bool,
    pub(super) detail: Option<ManagementPage>,
    pub(super) pending_pointer: Option<String>,
    pub(super) pending_cursor: Option<String>,
}

struct ManagementFrame {
    view: ManagementView,
    structured: Option<StructuredManagementState>,
}
#[derive(Default)]
pub(super) enum IndexPreviewState {
    #[default]
    Idle,
    Loading(IndexArgs),
    Ready(IndexArgs, String),
    Stale(IndexArgs, String),
    Failed(IndexArgs, String),
    Hint(String),
}
impl IndexPreviewState {
    fn ready(&self, args: &IndexArgs) -> Option<&str> {
        match self {
            Self::Ready(selected, text) if selected == args => Some(text),
            _ => None,
        }
    }
}
pub struct ManagementPopup {
    language: UiLanguage,
    pub(super) inspection: Option<inspection::State>,
    inspection_detail: Option<UiAction>,
    paint: std::cell::Cell<Option<popup::PaintBounds>>,
    reader_cache: std::cell::RefCell<popup::ReaderCache>,
    reader_revision: u64,
    frame: Option<ManagementFrame>,
    pub(super) owner: Option<ManagementContext>,
    pub(super) mode: ManagementMode,
    pub(super) epoch: u64,
    pub(super) scope: Option<IndexArgs>,
    pub(super) preview: IndexPreviewState,
    retained_preview: Option<(IndexArgs, String)>,
    preview_attempt: u64,
    active_preview_attempt: Option<u64>,
    pub(super) ready_preview_attempt: Option<u64>,
    pub(super) pending: Option<Command>,
}
impl Default for ManagementPopup {
    fn default() -> Self {
        Self {
            language: UiLanguage::Ja,
            inspection: None,
            inspection_detail: None,
            paint: Default::default(),
            reader_cache: Default::default(),
            reader_revision: 0,
            frame: None,
            owner: None,
            mode: ManagementMode::General,
            epoch: 0,
            scope: None,
            preview: IndexPreviewState::Idle,
            retained_preview: None,
            preview_attempt: 0,
            active_preview_attempt: None,
            ready_preview_attempt: None,
            pending: None,
        }
    }
}
pub(super) enum Outcome {
    Original(String),
    Actions(Vec<UiAction>),
    Close,
    Operations,
    Confirm {
        command: Box<Command>,
        detail: String,
        attempt: u64,
    },
}
impl ManagementPopup {
    pub(super) fn set_language(&mut self, language: UiLanguage) {
        if self.language != language {
            self.language = language;
            let mut reader = self.reader_cache.borrow_mut();
            reader.set_language(language);
            reader.clear();
        }
    }
    fn detail_footer(&self) -> String {
        crate::ui::text_width::glyphs::controls(local!(
            self.language,
            "Esc 戻る | ^v/PgUp/PgDn 閲覧\nN 次page | P 先頭 | C このpageをコピー",
            "Esc Back | ^v/PgUp/PgDn Scroll\nN Next page | P First page | C Copy this page"
        ))
    }
    pub(super) fn start_inspection(&mut self, target: InspectionTarget, title: String) {
        // Pending cost intents belong to their exact Index selection. Admitted
        // operations are tracked elsewhere and are never cancelled here.
        let same_index_scope = matches!(&target,InspectionTarget::Index(args) if self.mode==ManagementMode::Index && self.scope.as_ref().map_or_else(||args==&IndexArgs::default(),|current|current==args));
        if !same_index_scope {
            self.pending = None;
            self.reset_preview();
            self.scope = None;
        }
        self.open_structured(title, 0, vec![], false, false);
        if let InspectionTarget::Index(args) = &target {
            self.mode = ManagementMode::Index;
            self.scope = Some(args.clone());
        } else if matches!(target, InspectionTarget::Attachments { .. }) {
            self.mode = ManagementMode::Attachments;
        } else {
            self.mode = ManagementMode::General;
        }
        let desired = match &target {
            InspectionTarget::Attachments {
                thread_id,
                attachment_id: Some(id),
            } => Some(InspectionFieldAnchor {
                pointer: String::new(),
                entity: Some(("attachment_selection".into(), format!("{thread_id}/{id}"))),
                relative_pointer: "/attachment_id".into(),
                stable: true,
            }),
            _ => None,
        };
        let mut state = inspection::State::new(target);
        state.unavailable_selection = desired;
        self.inspection = Some(state);
        self.inspection_detail = None;
    }
    pub(super) fn start_retained_inspection(
        &mut self,
        request_id: u64,
        target: InspectionTarget,
    ) -> bool {
        let Some(state) = self
            .frame
            .as_mut()
            .and_then(|frame| frame.structured.as_mut())
            .filter(|state| state.request_id == request_id)
        else {
            return false;
        };
        if !target.retains_history() {
            return false;
        }
        let mut inspection = inspection::State::new(target);
        let mut fields = state.fields.clone();
        fields.truncate(256);
        let sep = crate::ui::text_width::glyphs::separator();
        for field in &mut fields {
            field.label = match self.language {
                UiLanguage::En => format!("At capture{sep}{}", field.label),
                UiLanguage::Ja => format!("取得時{sep}{}", field.label),
            };
        }
        inspection.anchors = fields
            .iter()
            .map(|field| InspectionFieldAnchor {
                pointer: field.key.clone(),
                entity: None,
                relative_pointer: field.key.clone(),
                stable: true,
            })
            .collect();
        state.fields = fields.clone();
        inspection.retained_fields = Some(fields);
        inspection.retained_omitted = state.omitted;
        self.inspection = Some(inspection);
        true
    }
    pub(super) fn patch_inspection(
        &mut self,
        request_id: u64,
        mut snapshot: InspectionSnapshot,
    ) -> bool {
        let language = self.language;
        let Some(inspection) = self.inspection.as_ref() else {
            return false;
        };
        if (inspection.has_snapshot
            && self
                .structured()
                .is_some_and(|state| request_id <= state.request_id))
            || inspection.target != snapshot.target
            || snapshot.metadata_sequence < inspection.metadata_sequence
            || inspection
                .usage_sequence
                .is_some_and(|old| snapshot.usage_sequence.is_none_or(|new| new < old))
            || snapshot.fields.len() > 256
            || snapshot.anchors.len() > 256
        {
            return false;
        }
        if let Some(retained) = &inspection.retained_fields {
            let available = 256usize.saturating_sub(snapshot.fields.len());
            snapshot.omitted |= inspection.retained_omitted || retained.len() > available;
            for field in retained.iter().take(available) {
                snapshot.fields.push(field.clone());
                snapshot.anchors.push(InspectionFieldAnchor {
                    pointer: field.key.clone(),
                    entity: None,
                    relative_pointer: field.key.clone(),
                    stable: true,
                });
            }
        }
        let selected = if inspection.reset_selection {
            None
        } else {
            inspection.unavailable_selection.clone().or_else(|| {
                self.structured()
                    .and_then(|state| state.fields.get(state.selected))
                    .and_then(|field| {
                        inspection
                            .anchors
                            .iter()
                            .find(|anchor| anchor.pointer == field.key)
                    })
                    .cloned()
            })
        };
        let selected_index = selected
            .as_ref()
            .and_then(|old| snapshot.anchors.iter().find(|new| old.same_source(new)))
            .and_then(|anchor| {
                snapshot
                    .fields
                    .iter()
                    .position(|field| field.key == anchor.pointer)
            });
        let detail_anchor = inspection.detail_anchor.clone();
        let retained_pointer = detail_anchor.as_ref().and_then(|anchor| {
            inspection
                .retained_fields
                .as_ref()?
                .iter()
                .find(|field| field.key == anchor.pointer)
                .map(|field| field.key.clone())
        });
        let mapped = retained_pointer.clone().or_else(|| {
            detail_anchor
                .as_ref()
                .and_then(|old| snapshot.anchors.iter().find(|new| old.same_source(new)))
                .map(|anchor| anchor.pointer.clone())
        });
        let Some(frame) = self.frame.as_mut() else {
            return false;
        };
        let Some(state) = frame.structured.as_mut() else {
            return false;
        };
        // The visible original page may coexist with an in-flight next/first
        // page request. Keep that exact intent before the metadata fence moves.
        let pending_original = state.pending_pointer.as_ref().and_then(|pointer| {
            inspection
                .retained_fields
                .as_ref()?
                .iter()
                .any(|field| &field.key == pointer)
                .then(|| {
                    (
                        pointer.clone(),
                        state.pending_cursor.clone(),
                        state.preserve_detail_scroll,
                    )
                })
        });
        let keep_original_page = retained_pointer.as_ref().is_some_and(|pointer| {
            state
                .detail
                .as_ref()
                .is_some_and(|page| &page.pointer == pointer)
        });
        let detail_consumer = inspection.detail_requested
            || state.detail.is_some()
            || state.pending_pointer.is_some()
            || self.inspection_detail.is_some();
        state.selected = selected_index
            .unwrap_or_else(|| state.selected.min(snapshot.fields.len().saturating_sub(1)));
        state.fields = snapshot.fields;
        state.omitted = snapshot.omitted;
        state.request_id = request_id;
        state.detail_request_id = state.detail_request_id.wrapping_add(1);
        state.pending_pointer = None;
        state.pending_cursor = None;
        self.inspection_detail = None;
        let inspection = self.inspection.as_mut().unwrap();
        inspection.reset_selection = false;
        inspection.selection_unavailable = selected.is_some() && selected_index.is_none();
        inspection.unavailable_selection = inspection
            .selection_unavailable
            .then(|| selected.clone())
            .flatten();
        inspection.metadata_sequence = snapshot.metadata_sequence;
        inspection.usage_sequence = snapshot.usage_sequence;
        inspection.metadata_observed_at = snapshot.metadata_observed_at;
        inspection.usage_observed_at = snapshot.usage_observed_at;
        inspection.anchors = snapshot.anchors;
        inspection.has_snapshot = true;
        if inspection
            .metadata_error
            .as_ref()
            .is_some_and(|(seq, _)| *seq < snapshot.metadata_sequence)
        {
            inspection.metadata_error = None;
        }
        if inspection
            .usage_error
            .as_ref()
            .is_some_and(|(seq, _)| snapshot.usage_sequence.is_some_and(|new| new > *seq))
        {
            inspection.usage_error = None;
        }
        inspection.detail_pointer = mapped.clone();
        if keep_original_page {
            let page = state
                .detail
                .as_mut()
                .expect("retained page identity checked above");
            page.source_request_id = request_id;
            page.stale = None;
        } else if let Some(detail) = state.detail.as_mut() {
            detail.stale = Some(
                if mapped.is_some() {
                    local!(
                        language,
                        "新しい保存状態の詳細を取得中",
                        "Retrieving details for the latest saved state"
                    )
                } else {
                    local!(
                        language,
                        "参照元が消失・一意に確認できません",
                        "The source is missing or cannot be identified uniquely"
                    )
                }
                .into(),
            );
        }
        if let Some((pointer, cursor, preserve_scroll)) = pending_original {
            state.pending_pointer = Some(pointer.clone());
            state.pending_cursor = cursor.clone();
            state.preserve_detail_scroll = preserve_scroll;
            self.inspection_detail = Some(UiAction::LoadManagementDetail {
                request_id,
                detail_request_id: state.detail_request_id,
                pointer,
                cursor,
            });
        } else if detail_consumer
            && !keep_original_page
            && let Some(pointer) = mapped
        {
            state.pending_pointer = Some(pointer.clone());
            state.pending_cursor = None;
            state.preserve_detail_scroll = state.detail.is_some();
            self.inspection_detail = Some(UiAction::LoadManagementDetail {
                request_id,
                detail_request_id: state.detail_request_id,
                pointer,
                cursor: None,
            });
        }
        self.paint.set(None);
        true
    }
    pub(super) fn fail_inspection(
        &mut self,
        target: InspectionTarget,
        part: InspectionPart,
        sequence: u64,
        observed_at: String,
        error: String,
    ) -> bool {
        let Some(state) = self
            .inspection
            .as_mut()
            .filter(|state| state.target == target)
        else {
            return false;
        };
        let prefix = error.chars().take(512).collect::<String>();
        let message = crate::ui::text_width::glyphs::formatted!(
            "{} · {}",
            "{} | {}",
            observed_at,
            crate::presentation::sanitize_plain(&prefix, 0).0
        );
        match part {
            InspectionPart::Metadata => {
                if sequence < state.metadata_sequence
                    || state
                        .metadata_error
                        .as_ref()
                        .is_some_and(|(old, _)| sequence < *old)
                {
                    return false;
                }
                state.metadata_error = Some((sequence, message));
            }
            InspectionPart::Usage => {
                if state.usage_sequence.is_some_and(|old| sequence < old)
                    || state
                        .usage_error
                        .as_ref()
                        .is_some_and(|(old, _)| sequence < *old)
                {
                    return false;
                }
                state.usage_error = Some((sequence, message));
            }
        }
        if let Some(page) = self
            .frame
            .as_mut()
            .and_then(|frame| frame.structured.as_mut())
            .and_then(|state| state.detail.as_mut())
        {
            page.stale = Some(
                local!(
                    self.language,
                    "更新に失敗・以前の取得内容",
                    "Refresh failed; showing the previous capture"
                )
                .into(),
            );
        }
        true
    }
    pub(super) fn fail_detail(
        &mut self,
        request_id: u64,
        detail_request_id: u64,
        pointer: &str,
        error: &str,
    ) -> bool {
        let language = self.language;
        let Some(state) = self.structured_mut() else {
            return false;
        };
        if state.request_id != request_id
            || state.detail_request_id != detail_request_id
            || state.pending_pointer.as_deref() != Some(pointer)
        {
            return false;
        }
        let prefix = error.chars().take(512).collect::<String>();
        let reason = crate::presentation::sanitize_plain(&prefix, 0).0;
        state.pending_pointer = None;
        state.pending_cursor = None;
        state.detail_error = Some(reason.clone());
        if let Some(page) = &mut state.detail {
            page.stale = Some(match language {
                UiLanguage::En => format!("Failed to refresh details: {reason}"),
                UiLanguage::Ja => format!("詳細の更新に失敗: {reason}"),
            });
        }
        true
    }
    pub(super) fn take_inspection_detail(&mut self) -> Option<UiAction> {
        self.inspection_detail.take()
    }
    pub(super) fn detail_rejected(&mut self, reason: &str) {
        if let Some(state) = self.structured_mut() {
            state.pending_pointer = None;
            state.pending_cursor = None;
            if let Some(page) = &mut state.detail {
                page.stale = Some(reason.into());
            }
        }
        self.inspection_detail = None;
    }
    pub(super) fn paint_bounds(&self) -> Option<popup::PaintBounds> {
        self.paint.get()
    }
    pub(super) fn wheel(&mut self, delta: isize) -> bool {
        if let Some(state) = self.structured_mut()
            && state.detail.is_none()
        {
            let old = state.selected;
            state.selected = popup::move_selection(old, delta, state.fields.len());
            let changed = old != state.selected;
            if changed && let Some(inspection) = self.inspection.as_mut() {
                inspection.selection_unavailable = false;
                inspection.unavailable_selection = None;
            }
            return changed;
        }
        let maximum = self.reader_cache.borrow().bounds().maximum;
        if let Some(view) = self.view_mut() {
            let old = view.scroll;
            view.scroll = popup::move_reader(old, delta, maximum);
            return old != view.scroll;
        }
        false
    }
    fn reset_reader(&mut self) {
        self.paint.set(None);
        self.reader_cache.get_mut().clear();
        self.reader_revision = self.reader_revision.wrapping_add(1);
    }
    pub fn is_some(&self) -> bool {
        self.frame.is_some()
    }
    pub fn is_none(&self) -> bool {
        self.frame.is_none()
    }
    pub fn as_ref(&self) -> Option<&ManagementView> {
        self.frame.as_ref().map(|frame| &frame.view)
    }
    pub(super) fn view_mut(&mut self) -> Option<&mut ManagementView> {
        self.frame.as_mut().map(|frame| &mut frame.view)
    }
    pub(super) fn structured(&self) -> Option<&StructuredManagementState> {
        self.frame.as_ref()?.structured.as_ref()
    }
    pub(super) fn structured_mut(&mut self) -> Option<&mut StructuredManagementState> {
        self.frame.as_mut()?.structured.as_mut()
    }
    pub(super) fn open_text(&mut self, view: ManagementView) {
        self.inspection = None;
        self.inspection_detail = None;
        self.reset_reader();
        self.frame = Some(ManagementFrame {
            view,
            structured: None,
        });
    }
    pub(super) fn open_structured(
        &mut self,
        title: String,
        request_id: u64,
        fields: Vec<PresentationField>,
        omitted: bool,
        work_status: bool,
    ) {
        self.inspection = None;
        self.inspection_detail = None;
        self.reset_reader();
        if work_status {
            self.mode = ManagementMode::Status;
        } else if self.mode == ManagementMode::Status {
            self.mode = ManagementMode::General;
        }
        self.frame = Some(ManagementFrame {
            view: ManagementView {
                title,
                text: String::new(),
                scroll: 0,
            },
            structured: Some(StructuredManagementState {
                detail_request_id: 0,
                preserve_detail_scroll: false,
                detail_error: None,
                request_id,
                fields,
                selected: 0,
                omitted,
                detail: None,
                pending_pointer: None,
                pending_cursor: None,
            }),
        });
    }
    pub(super) fn accept_detail(
        &mut self,
        request_id: u64,
        detail_request_id: u64,
        pointer: String,
        text: String,
        next_cursor: Option<String>,
    ) {
        if let Some(frame) = &mut self.frame
            && let Some(state) = &mut frame.structured
            && state.request_id == request_id
            && state.detail_request_id == detail_request_id
            && state.pending_pointer.as_deref() == Some(&pointer)
        {
            self.paint.set(None);
            self.reader_cache.get_mut().clear();
            self.reader_revision = self.reader_revision.wrapping_add(1);
            state.detail = Some(ManagementPage {
                source_request_id: request_id,
                observed_at: self
                    .inspection
                    .as_ref()
                    .map(|state| match &state.target {
                        InspectionTarget::HistorySources(scope)
                            if pointer != "/current_source"
                                && !pointer.starts_with("/current_source/") =>
                        {
                            scope.captured_at.clone()
                        }
                        InspectionTarget::RetainedSources(scope)
                            if pointer != "/current_source"
                                && !pointer.starts_with("/current_source/") =>
                        {
                            scope.captured_at.to_string()
                        }
                        _ => state.metadata_observed_at.clone(),
                    })
                    .unwrap_or_default(),
                stale: None,
                pointer,
                text,
                next_cursor,
            });
            state.pending_pointer = None;
            state.pending_cursor = None;
            state.detail_error = None;
            if !state.preserve_detail_scroll {
                frame.view.scroll = 0;
            }
            state.preserve_detail_scroll = false;
        }
    }
    pub(super) fn append_text(&mut self, title: String, text: String) {
        let mut body = self
            .frame
            .take()
            .map(|frame| frame.view.text)
            .unwrap_or_default();
        body.push_str(&format!("\n\n{title}\n{text}"));
        if body.len() > 65536 {
            let start = body.ceil_char_boundary(body.len() - 65536);
            body.drain(..start);
        }
        self.open_text(ManagementView {
            title,
            text: body,
            scroll: 0,
        });
    }
    pub(super) fn close(&mut self) {
        self.reset_reader();
        self.epoch = self.epoch.wrapping_add(1);
        self.frame = None;
        self.inspection = None;
        self.inspection_detail = None;
        self.paint.set(None);
        self.reader_cache.get_mut().clear();
        self.owner = None;
        self.mode = ManagementMode::General;
        self.scope = None;
        self.reset_preview();
        self.pending = None;
    }
    pub(super) fn accepts(
        &self,
        context: &ManagementContext,
        draft: &str,
        thread: Option<&str>,
    ) -> bool {
        self.owner.as_ref() == Some(context)
            && context.epoch == self.epoch
            && context
                .owner_thread
                .as_deref()
                .map_or(context.owner_draft == draft, |owner| Some(owner) == thread)
    }
    pub(super) fn reset_preview(&mut self) {
        self.preview = IndexPreviewState::Idle;
        self.retained_preview = None;
        self.active_preview_attempt = None;
        self.ready_preview_attempt = None;
    }
    pub(super) fn invalidate_index_preview(&mut self) {
        self.active_preview_attempt = None;
        self.ready_preview_attempt = None;
        self.pending = None;
        self.preview = match std::mem::take(&mut self.preview) {
            IndexPreviewState::Ready(args, text) => IndexPreviewState::Stale(args, text),
            IndexPreviewState::Loading(args) => {
                let text = self
                    .retained_preview
                    .as_ref()
                    .filter(|(previous, _)| previous == &args)
                    .map(|(_, text)| text.clone())
                    .unwrap_or_else(|| {
                        local!(
                            self.language,
                            "取得中の確認は無効になりました",
                            "The pending preview was invalidated"
                        )
                        .into()
                    });
                IndexPreviewState::Stale(args, text)
            }
            other => other,
        };
    }
    fn matches_preview(&self, args: &IndexArgs) -> bool {
        self.mode == ManagementMode::Index
            && self
                .scope
                .as_ref()
                .map_or_else(|| args == &IndexArgs::default(), |scope| scope == args)
    }
    pub(super) fn begin_preview(&mut self, args: IndexArgs) -> u64 {
        self.preview_attempt = self.preview_attempt.wrapping_add(1);
        let attempt = self.preview_attempt;
        if self.matches_preview(&args) {
            self.active_preview_attempt = Some(attempt);
            self.ready_preview_attempt = None;
            self.preview = IndexPreviewState::Loading(args);
        }
        attempt
    }
    fn pending_preview(&self, attempt: u64, args: &IndexArgs) -> bool {
        self.active_preview_attempt == Some(attempt)
            && self.matches_preview(args)
            && matches!(&self.preview, IndexPreviewState::Loading(pending) if pending == args)
    }
    pub(super) fn accept_preview(&mut self, attempt: u64, args: IndexArgs, text: String) -> bool {
        if !self.pending_preview(attempt, &args) {
            return false;
        }
        self.active_preview_attempt = None;
        self.ready_preview_attempt = Some(attempt);
        self.retained_preview = Some((args.clone(), text.clone()));
        self.preview = IndexPreviewState::Ready(args, text);
        true
    }
    pub(super) fn fail_preview(&mut self, attempt: u64, args: IndexArgs, message: String) {
        if self.pending_preview(attempt, &args) {
            self.active_preview_attempt = None;
            self.pending = None;
            self.preview = IndexPreviewState::Failed(args, message);
        }
    }
    fn preview_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.mode != ManagementMode::Index
            || self
                .structured()
                .is_some_and(|value| value.detail.is_some())
        {
            return vec![];
        }
        let (title, text) = match &self.preview {
            IndexPreviewState::Idle => return vec![],
            IndexPreviewState::Loading(args) => (
                if explicit_index_targets(args) {
                    local!(
                        self.language,
                        "費用preview | 取得中",
                        "Cost preview | Loading"
                    )
                } else {
                    local!(
                        self.language,
                        "再試行preview | 取得中",
                        "Retry preview | Loading"
                    )
                },
                local!(
                    self.language,
                    "件数と文字数を確認中 | API未送信",
                    "Checking counts and characters | API not called"
                ),
            ),
            IndexPreviewState::Ready(args, text) => (
                if explicit_index_targets(args) {
                    local!(
                        self.language,
                        "費用preview | 取得済み",
                        "Cost preview | Captured"
                    )
                } else {
                    local!(
                        self.language,
                        "再試行preview | 取得済み",
                        "Retry preview | Captured"
                    )
                },
                text.as_str(),
            ),
            IndexPreviewState::Stale(args, text) => (
                if explicit_index_targets(args) {
                    local!(
                        self.language,
                        "古い費用preview | 再取得が必要",
                        "Stale cost preview | Refresh required"
                    )
                } else {
                    local!(
                        self.language,
                        "古い再試行preview | 再取得が必要",
                        "Stale retry preview | Refresh required"
                    )
                },
                text.as_str(),
            ),
            IndexPreviewState::Failed(args, text) => (
                if explicit_index_targets(args) {
                    local!(
                        self.language,
                        "費用preview | 取得失敗",
                        "Cost preview | Failed"
                    )
                } else {
                    local!(
                        self.language,
                        "再試行preview | 取得失敗",
                        "Retry preview | Failed"
                    )
                },
                text.as_str(),
            ),
            IndexPreviewState::Hint(text) => (
                local!(
                    self.language,
                    "索引操作 | 未受付",
                    "Index operation | Not submitted"
                ),
                text.as_str(),
            ),
        };
        let mut lines = vec![
            Line::from(crate::ui::text_width::glyphs::decorate(title))
                .style(Style::default().fg(theme::EMPHASIS)),
        ];
        let text = if matches!(self.preview, IndexPreviewState::Loading(_)) {
            crate::ui::text_width::glyphs::decorate(text)
        } else {
            text.to_owned()
        };
        let safe: String = crate::presentation::sanitize_plain(&text, 0)
            .0
            .chars()
            .take(4096)
            .collect();
        if let IndexPreviewState::Failed(args, _) = &self.preview
            && let Some((_, previous)) = self
                .retained_preview
                .as_ref()
                .filter(|(saved, _)| saved == args)
        {
            // Keep a long error from pushing the retained result out of view.
            let error = safe.replace('\n', " ");
            let short: String = error.chars().take(160).collect();
            lines.push(Line::from(if error.chars().count() > 160 {
                format!("{short}{}", crate::ui::text_width::glyphs::ellipsis())
            } else {
                short
            }));
            lines.push(Line::from(crate::ui::text_width::glyphs::decorate(local!(
                self.language,
                "前回の結果 | 古い情報（再確認が必要）",
                "Previous result | Stale (review again)"
            ))));
            let safe: String = crate::presentation::sanitize_plain(previous, 0)
                .0
                .chars()
                .take(4096)
                .collect();
            lines.extend(safe.lines().map(|line| Line::from(line.to_owned())));
        } else {
            lines.extend(safe.lines().map(|line| Line::from(line.to_owned())));
        }
        presentation::hanging_wrap(lines, width.max(1))
    }
    fn preview_height(&self, area: Rect) -> u16 {
        let count = self.preview_lines(area.width).len();
        if count == 0 || area.height == 0 {
            return 0;
        }
        u16::try_from(count)
            .unwrap_or(u16::MAX)
            .min(area.height.saturating_sub(3).max(1))
            .min(12)
    }
    fn draw_preview(&self, frame: &mut Frame<'_>, area: Rect) -> Rect {
        let height = self.preview_height(area);
        if height == 0 {
            return area;
        }
        let mut lines = self.preview_lines(area.width);
        mark_truncated_preview(&mut lines, height, self.language);
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(area.x, area.y, area.width, height),
        );
        Rect::new(
            area.x,
            area.y.saturating_add(height),
            area.width,
            area.height.saturating_sub(height),
        )
    }
    pub(super) fn index_label(&self) -> String {
        let space = self
            .scope
            .as_ref()
            .and_then(|args| args.space_id.as_deref())
            .unwrap_or("active");
        match (self.language, &self.scope) {
            (UiLanguage::En, Some(args)) if args.scope == Some(IndexScope::All) => {
                format!("space={space} / All conversations and retained sources (all)")
            }
            (UiLanguage::En, Some(args)) if !args.thread_ids.is_empty() => {
                let ids = args.thread_ids.join(", ");
                format!("space={space} / Thread IDs: {ids}")
            }
            (UiLanguage::En, _) => format!(
                "space={space} / No target selected. Use /index all or /index threads ID..."
            ),
            (UiLanguage::Ja, Some(args)) if args.scope == Some(IndexScope::All) => {
                format!("space={space} / 全会話 / 保持された出典（all）")
            }
            (UiLanguage::Ja, Some(args)) if !args.thread_ids.is_empty() => {
                let ids = args.thread_ids.join(", ");
                format!("space={space} / 会話ID: {ids}")
            }
            (UiLanguage::Ja, _) => {
                format!("space={space} / 対象未選択。/index all または /index threads ID...")
            }
        }
    }
    fn has_next_inspection_window(&self) -> bool {
        self.inspection
            .as_ref()
            .is_some_and(|inspection| matches!(inspection.target, InspectionTarget::Index(_)))
            && self.structured().is_some_and(|state| {
                state.fields.iter().any(|field| {
                    field.key == "/management/coverage_window/next_cursor"
                        && !field.value.is_empty()
                        && field.value != "null"
                })
            })
    }
    fn hints(&self) -> String {
        use super::i18n::local;
        let list_mode = self
            .structured()
            .is_some_and(|value| value.detail.is_none());
        crate::ui::text_width::glyphs::controls(match self.mode {
            ManagementMode::Index if self.has_next_inspection_window() => local!(
                self.language,
                "Esc 戻る | ^v/PgUp/PgDn 閲覧 | Ctrl+R 更新 | N 次window\nS 状態 | P 費用preview | B backfill | T retry | R rebuild",
                "Esc Back | ^v/PgUp/PgDn Scroll | Ctrl+R Refresh | N Next window\nS Status | P Cost preview | B Backfill | T Retry | R Rebuild"
            ),
            ManagementMode::Index => local!(
                self.language,
                "Esc 戻る | ^v/PgUp/PgDn 閲覧 | Ctrl+R 更新\nS 状態 | P 費用preview | B backfill | T retry | R rebuild",
                "Esc Back | ^v/PgUp/PgDn Scroll | Ctrl+R Refresh\nS Status | P Cost preview | B Backfill | T Retry | R Rebuild"
            ),
            ManagementMode::Attachments if list_mode => local!(
                self.language,
                "Esc 戻る | Tab 選択 | Enter 詳細 | C コピー\nS 保存済み一覧 | R 再読込 | 選択解除は戻って /detach ID",
                "Esc Back | Tab Select | Enter Details | C Copy\nS Saved list | R Reload | Return and use /detach ID to deselect"
            ),
            ManagementMode::Attachments => local!(
                self.language,
                "Esc 戻る | ^v/PgUp/PgDn 閲覧\nS 保存済み一覧 | R 再読込 | 解除は戻って /detach ID",
                "Esc Back | ^v/PgUp/PgDn Scroll\nS Saved list | R Reload | Return and use /detach ID to deselect"
            ),
            ManagementMode::Status if list_mode => local!(
                self.language,
                "Esc 戻る | ^v 選択 | Enter 詳細 | R 更新 | N 次page\nO 参照 | S 出力原文 | C コピー | A 操作",
                "Esc Back | ^v Select | Enter Details | R Refresh | N Next page\nO Reference | S Original output | C Copy | A Actions"
            ),
            ManagementMode::General if list_mode => local!(
                self.language,
                "Esc 戻る | Tab/Shift+Tab 選択 | Enter 詳細 | C コピー\nO 参照 | S 出力原文 | A 操作 | PgUp/PgDn 閲覧",
                "Esc Back | Tab/Shift+Tab Select | Enter Details | C Copy\nO Reference | S Original output | A Actions | PgUp/PgDn Scroll"
            ),
            ManagementMode::Received => local!(
                self.language,
                "Esc 戻る | ^v 選択 | Enter 原文／ページ | C IDコピー\n/received ページ番号 で全受信記録を表示",
                "Esc Back | ^v Select | Enter Original / page | C Copy ID\n/received PAGE shows all received records"
            ),
            ManagementMode::General | ManagementMode::Status => local!(
                self.language,
                "Esc 戻る | ^v/PgUp/PgDn 閲覧\nW 明示waitの観測取消 | A 操作",
                "Esc Back | ^v/PgUp/PgDn Scroll\nW Stop observing explicit wait | A Actions"
            ),
        })
    }
    pub(super) fn page(&self, viewport: Rect, status: &str) -> u16 {
        let detailed = self
            .structured()
            .is_some_and(|value| value.detail.is_some());
        let context = if self.structured().is_some() || self.mode == ManagementMode::Index {
            local!(self.language, "項目", "Fields")
        } else {
            status
        };
        let footer = if detailed {
            self.detail_footer()
        } else {
            self.hints()
        };
        let body = popup::body_area(viewport, context, &footer, None);
        body.height
            .saturating_sub(self.preview_height(body))
            .saturating_sub(1)
            .max(1)
    }
    pub(super) fn key(&mut self, key: KeyEvent, page: u16, thread_id: Option<String>) -> Outcome {
        let key = key_hints::readonly_key(key);
        let refresh = matches!(key.code, KeyCode::Char('r' | 'R'))
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || (!matches!(
                    self.mode,
                    ManagementMode::Index | ManagementMode::Attachments
                ) && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)));
        if refresh && let (Some(inspection), Some(context)) = (&self.inspection, &self.owner) {
            return Outcome::Actions(vec![UiAction::ObserveInspection {
                context: context.clone(),
                target: inspection.target.clone(),
                visible: true,
                revision: 0,
            }]);
        }
        if key.code == KeyCode::Char('n')
            && key.modifiers.is_empty()
            && self.mode == ManagementMode::Index
            && self
                .structured()
                .is_some_and(|state| state.detail.is_none())
            && self.has_next_inspection_window()
        {
            let (Some(context), Some(inspection), Some(state)) = (
                self.owner.clone(),
                self.inspection.as_mut(),
                self.frame
                    .as_mut()
                    .and_then(|frame| frame.structured.as_mut()),
            ) else {
                return Outcome::Actions(vec![]);
            };
            let target = inspection.target.clone();
            let request_id = state.request_id;
            inspection.reset_selection = true;
            inspection.selection_unavailable = false;
            inspection.unavailable_selection = None;
            inspection.detail_anchor = None;
            inspection.detail_pointer = None;
            inspection.detail_requested = false;
            state.selected = 0;
            state.pending_pointer = None;
            state.pending_cursor = None;
            state.detail_request_id = state.detail_request_id.wrapping_add(1);
            self.inspection_detail = None;
            self.paint.set(None);
            return Outcome::Actions(vec![UiAction::NextInspectionWindow {
                context,
                request_id,
                target,
            }]);
        }
        if self.mode == ManagementMode::Received
            && key.code == KeyCode::Enter
            && let Some(field) = self.structured().and_then(|s| s.fields.get(s.selected))
        {
            if field.key.starts_with("/received/source/") {
                return Outcome::Original(field.value.clone());
            }
            if let Some(page) = field
                .key
                .strip_prefix("/received/page/")
                .and_then(|p| p.parse::<u32>().ok())
            {
                return Outcome::Actions(vec![UiAction::LoadReceived {
                    thread_id: field.value.clone(),
                    page,
                }]);
            }
        }

        if key.code == KeyCode::Char('w') {
            return Outcome::Actions(vec![UiAction::CancelWait]);
        }
        if key.code == KeyCode::Char('a') {
            return Outcome::Operations;
        }
        if key.code == KeyCode::Esc && self.structured().is_none_or(|value| value.detail.is_none())
        {
            return Outcome::Close;
        }
        if self.mode == ManagementMode::Index
            && let KeyCode::Char(action @ ('b' | 't' | 'r')) = key.code
        {
            let args = self.scope.clone().unwrap_or_default();
            if action != 't' && !explicit_index_targets(&args) {
                self.pending = None;
                self.preview = IndexPreviewState::Hint(
                    local!(
                        self.language,
                        "対象を明示してください。Esc後 /index all または /index thread",
                        "Specify a target. After Esc, use /index all or /index thread"
                    )
                    .into(),
                );
                return Outcome::Actions(vec![]);
            }
            let preview = self.preview.ready(&args).map(str::to_owned);
            let Some(preview) = preview else {
                if action == 't' {
                    self.pending = Some(Command::IndexRetry(args.clone()));
                    let attempt = self.begin_preview(args.clone());
                    return Outcome::Actions(vec![UiAction::IndexPreview { args, attempt }]);
                }
                if !matches!(self.preview, IndexPreviewState::Loading(_)) {
                    self.preview = IndexPreviewState::Hint(
                        local!(
                            self.language,
                            "現在の対象のpreviewが必要です。Pで取得してください。",
                            "A preview of the current target is required. Press P to retrieve it."
                        )
                        .into(),
                    );
                }
                return Outcome::Actions(vec![]);
            };
            let Some(attempt) = self.ready_preview_attempt else {
                self.preview = IndexPreviewState::Hint(
                    local!(
                        self.language,
                        "現在の対象のpreviewが必要です。Pで取得してください。",
                        "A preview of the current target is required. Press P to retrieve it."
                    )
                    .into(),
                );
                return Outcome::Actions(vec![]);
            };
            let command = match action {
                'b' => Command::IndexBackfill(args.clone()),
                't' => Command::IndexRetry(args.clone()),
                _ => Command::IndexRebuild(args.clone()),
            };
            let target = if action == 't' && !explicit_index_targets(&args) {
                local!(
                    self.language,
                    "失敗した許可済み索引作業（全体）",
                    "Failed authorized index work (all)"
                )
                .to_owned()
            } else {
                self.index_label()
            };
            let detail = match self.language {
                UiLanguage::En => format!(
                    "Cost target: {target}\n{preview}\nThis sends data to an external embedding API and incurs costs. Counts reflect the capture; monetary cost is not estimated."
                ),
                UiLanguage::Ja => format!(
                    "費用対象: {target}\n{preview}\n外部embedding APIへ送信し費用が発生します。見積りは取得時点の対象数で、金額未見積です。"
                ),
            };
            Outcome::Confirm {
                command: Box::new(command),
                attempt,
                detail,
            }
        } else {
            Outcome::Actions(self.navigate(key, page, thread_id))
        }
    }
    fn navigate(&mut self, key: KeyEvent, page: u16, thread_id: Option<String>) -> Vec<UiAction> {
        let has_detail = self
            .structured()
            .is_some_and(|state| state.detail.is_some());
        if let Some(inspection) = self.inspection.as_mut() {
            if has_detail && key.code == KeyCode::Esc {
                inspection.detail_requested = false;
                inspection.detail_anchor = None;
                inspection.detail_pointer = None;
            }
            if !has_detail
                && matches!(
                    key.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                        | KeyCode::Tab
                        | KeyCode::BackTab
                )
            {
                inspection.selection_unavailable = false;
                inspection.unavailable_selection = None;
            }
            if inspection.selection_unavailable
                && (matches!(key.code, KeyCode::Enter | KeyCode::Char('o' | 's'))
                    || (!has_detail && key.code == KeyCode::Char('c')))
            {
                return vec![];
            }
        }
        let starts_field_detail = !has_detail
            && (key.code == KeyCode::Enter
                || (key.code == KeyCode::Char('c')
                    && self
                        .structured()
                        .and_then(|state| state.fields.get(state.selected))
                        .is_some_and(|field| field.value_omitted)));
        if starts_field_detail {
            let anchor = self
                .structured()
                .and_then(|state| state.fields.get(state.selected))
                .and_then(|field| {
                    self.inspection.as_ref().and_then(|inspection| {
                        inspection
                            .anchors
                            .iter()
                            .find(|anchor| anchor.pointer == field.key)
                    })
                })
                .cloned();
            if let Some(inspection) = self.inspection.as_mut() {
                inspection.detail_requested = true;
                inspection.detail_pointer = anchor.as_ref().map(|anchor| anchor.pointer.clone());
                inspection.detail_anchor = anchor;
            }
        }
        let inspection_pointer = self
            .inspection
            .as_ref()
            .and_then(|state| state.detail_pointer.clone());
        let is_status = self.mode == ManagementMode::Status;
        let is_index = self.mode == ManagementMode::Index;
        let is_attachments = self.mode == ManagementMode::Attachments;
        if let Some(structured) = self.structured_mut() {
            if structured.detail.is_none() {
                let mode = match key.code {
                    KeyCode::Char('o') => Some(ManagementFollow::Inspect),
                    KeyCode::Char('s') if !is_index && !is_attachments => {
                        Some(ManagementFollow::Read)
                    }
                    KeyCode::Char('r') if is_status => Some(ManagementFollow::Refresh),
                    KeyCode::Char('n') if is_status => Some(ManagementFollow::Next),
                    _ => None,
                };
                if let Some(mode) = mode {
                    return vec![UiAction::FollowManagement {
                        request_id: structured.request_id,
                        pointer: structured
                            .fields
                            .get(structured.selected)
                            .map(|field| field.key.clone())
                            .unwrap_or_default(),
                        mode,
                    }];
                }
            }
            match key.code {
                KeyCode::Tab | KeyCode::Down if structured.detail.is_none() => {
                    structured.selected =
                        (structured.selected + 1).min(structured.fields.len().saturating_sub(1));
                    return vec![];
                }
                KeyCode::BackTab | KeyCode::Up if structured.detail.is_none() => {
                    structured.selected = structured.selected.saturating_sub(1);
                    return vec![];
                }
                KeyCode::PageDown if structured.detail.is_none() => {
                    structured.selected = structured
                        .selected
                        .saturating_add(usize::from(page))
                        .min(structured.fields.len().saturating_sub(1));
                    return vec![];
                }
                KeyCode::PageUp if structured.detail.is_none() => {
                    structured.selected = structured.selected.saturating_sub(usize::from(page));
                    return vec![];
                }
                KeyCode::Tab if structured.detail.is_none() => {
                    structured.selected =
                        (structured.selected + 1).min(structured.fields.len().saturating_sub(1));
                    return vec![];
                }
                KeyCode::BackTab if structured.detail.is_none() => {
                    structured.selected = structured.selected.saturating_sub(1);
                    return vec![];
                }
                KeyCode::Char('c') => {
                    if let Some(page) = &structured.detail {
                        return vec![UiAction::CopyText {
                            text: page.text.clone(),
                            scope: CopyScope::ManagementPage {
                                has_more: page.next_cursor.is_some(),
                            },
                        }];
                    }
                    if let Some(field) = structured.fields.get(structured.selected) {
                        if field.value_omitted {
                            structured.detail_request_id =
                                structured.detail_request_id.wrapping_add(1);
                            structured.preserve_detail_scroll = false;
                            structured.pending_pointer = Some(field.key.clone());
                            structured.pending_cursor = None;
                            return vec![UiAction::LoadManagementDetail {
                                request_id: structured.request_id,
                                detail_request_id: structured.detail_request_id,
                                pointer: field.key.clone(),
                                cursor: None,
                            }];
                        }
                        return vec![UiAction::CopyText {
                            text: field.value.clone(),
                            scope: CopyScope::Value,
                        }];
                    }
                }
                KeyCode::Enter => {
                    if let Some(field) = structured.fields.get(structured.selected) {
                        structured.detail_request_id = structured.detail_request_id.wrapping_add(1);
                        structured.preserve_detail_scroll = false;
                        structured.pending_pointer = Some(field.key.clone());
                        structured.pending_cursor = None;
                        return vec![UiAction::LoadManagementDetail {
                            request_id: structured.request_id,
                            detail_request_id: structured.detail_request_id,
                            pointer: field.key.clone(),
                            cursor: None,
                        }];
                    }
                }
                KeyCode::Char('n') => {
                    if let Some(page) = &structured.detail
                        && page.source_request_id == structured.request_id
                        && let Some(cursor) = &page.next_cursor
                    {
                        structured.detail_request_id = structured.detail_request_id.wrapping_add(1);
                        structured.preserve_detail_scroll = false;
                        structured.pending_pointer = Some(page.pointer.clone());
                        structured.pending_cursor = Some(cursor.clone());
                        return vec![UiAction::LoadManagementDetail {
                            request_id: structured.request_id,
                            detail_request_id: structured.detail_request_id,
                            pointer: page.pointer.clone(),
                            cursor: Some(cursor.clone()),
                        }];
                    }
                }
                KeyCode::Char('p') => {
                    if let Some(page) = &structured.detail {
                        let pointer = inspection_pointer.clone().or_else(|| {
                            (page.source_request_id == structured.request_id)
                                .then(|| page.pointer.clone())
                        });
                        let Some(pointer) = pointer else {
                            return vec![];
                        };
                        structured.detail_request_id = structured.detail_request_id.wrapping_add(1);
                        structured.preserve_detail_scroll = false;
                        structured.pending_pointer = Some(pointer.clone());
                        structured.pending_cursor = None;
                        return vec![UiAction::LoadManagementDetail {
                            request_id: structured.request_id,
                            detail_request_id: structured.detail_request_id,
                            pointer,
                            cursor: None,
                        }];
                    }
                }
                KeyCode::Esc if structured.detail.is_some() => {
                    structured.detail = None;
                    structured.pending_pointer = None;
                    structured.pending_cursor = None;
                    structured.detail_request_id = structured.detail_request_id.wrapping_add(1);
                    return vec![];
                }
                _ => {}
            }
        }
        if !matches!(
            key.code,
            KeyCode::Esc | KeyCode::PageDown | KeyCode::Down | KeyCode::PageUp | KeyCode::Up
        ) && self.mode != management::ManagementMode::Index
        {
            if self.mode == management::ManagementMode::Attachments
                && let Some(thread_id) = self
                    .inspection
                    .as_ref()
                    .and_then(|inspection| match &inspection.target {
                        InspectionTarget::Attachments { thread_id, .. } => Some(thread_id.clone()),
                        _ => None,
                    })
                    .or_else(|| thread_id.clone())
            {
                return match key.code {
                    KeyCode::Char('s') => {
                        vec![UiAction::Manage(Command::AttachmentList(ThreadTarget {
                            thread_id: Some(thread_id),
                        }))]
                    }
                    KeyCode::Char('r') => vec![UiAction::ReloadAttachments { thread_id }],
                    _ => vec![],
                };
            }
            return vec![];
        }
        match key.code {
            KeyCode::PageDown | KeyCode::Down | KeyCode::PageUp | KeyCode::Up => {
                if let Some(view) = self.view_mut() {
                    view.scroll =
                        popup::reader_key(key.code, view.scroll, page).unwrap_or(view.scroll);
                }
                vec![]
            }
            KeyCode::Char('s') => {
                let args = self.scope.clone().unwrap_or_default();
                vec![UiAction::Manage(Command::IndexStatus(args))]
            }
            KeyCode::Char('p') => {
                let Some(args) = self.scope.clone().filter(explicit_index_targets) else {
                    self.pending = None;
                    self.preview = IndexPreviewState::Hint(local!(self.language, "対象を明示してください。Esc後 /index all または /index thread。失敗分の再試行はTで確認できます。", "Specify a target. After Esc, use /index all or /index thread. Press T to review retrying failed work.").into());
                    return vec![];
                };
                let attempt = self.begin_preview(args.clone());
                vec![UiAction::IndexPreview { args, attempt }]
            }
            _ => vec![],
        }
    }
    pub(super) fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        status: &str,
        language: crate::config::UiLanguage,
    ) {
        self.set_language(language);
        let sep = crate::ui::text_width::glyphs::separator();
        if let Some(content) = &self.frame {
            let view = &content.view;
            let hints = self.hints();
            let mut reader_scroll = None;
            if let Some(structured) = &content.structured {
                if let Some(page) = &structured.detail {
                    let title = match self.language {
                        UiLanguage::En => format!("{}{sep}Details", view.title),
                        UiLanguage::Ja => format!("{}{sep}詳細", view.title),
                    };
                    let pointer = &page.pointer;
                    let observed_at = &page.observed_at;
                    let context = match (self.language, &page.stale) {
                        (UiLanguage::En, Some(reason)) => format!(
                            "Reference {pointer}{sep}Captured {observed_at}{sep}Stale: {reason}"
                        ),
                        (UiLanguage::En, None) => {
                            format!("Reference {pointer}{sep}Captured {observed_at}")
                        }
                        (UiLanguage::Ja, Some(reason)) => {
                            format!("参照 {pointer}{sep}取得 {observed_at}{sep}旧内容: {reason}")
                        }
                        (UiLanguage::Ja, None) => format!("参照 {pointer}{sep}取得 {observed_at}"),
                    };
                    let body = popup::panel_recorded(
                        frame,
                        area,
                        &title,
                        &context,
                        &self.detail_footer(),
                        &self.paint,
                    );
                    reader_scroll = Some(self.reader_cache.borrow_mut().render(
                        frame,
                        body,
                        self.reader_revision,
                        view.scroll,
                        || {
                            crate::presentation::sanitize_plain(&page.text, 0)
                                .0
                                .lines()
                                .map(|line| Line::from(line.to_owned()))
                                .collect::<Vec<_>>()
                        },
                    ));
                } else {
                    let selected = if structured.fields.is_empty() {
                        0
                    } else {
                        structured.selected + 1
                    };
                    let count = structured.fields.len();
                    let status = match self.mode {
                        ManagementMode::Index => self.index_label(),
                        _ => status.to_owned(),
                    };
                    let context = match (self.language, structured.omitted, count) {
                        (UiLanguage::En, true, 1) => {
                            format!("{selected} / 1 field{sep}View all details{sep}{status}")
                        }
                        (UiLanguage::En, false, 1) => format!("{selected} / 1 field{sep}{status}"),
                        (UiLanguage::En, true, _) => {
                            format!("{selected} / {count} fields{sep}View all details{sep}{status}")
                        }
                        (UiLanguage::En, false, _) => {
                            format!("{selected} / {count} fields{sep}{status}")
                        }
                        (UiLanguage::Ja, true, _) => {
                            format!("{selected} / {count}項目{sep}すべての詳細を見る{sep}{status}")
                        }
                        (UiLanguage::Ja, false, _) => {
                            format!("{selected} / {count}項目{sep}{status}")
                        }
                    };
                    let context = if let Some(inspection) = &self.inspection {
                        format!("{}{sep}{context}", inspection.label_for(self.language))
                    } else {
                        context
                    };
                    let context = if let Some(error) = &structured.detail_error {
                        match self.language {
                            UiLanguage::En => {
                                format!("Details not refreshed: {error}{sep}{context}")
                            }
                            UiLanguage::Ja => format!("詳細未更新: {error}{sep}{context}"),
                        }
                    } else {
                        context
                    };
                    let body = popup::panel_recorded(
                        frame,
                        area,
                        &view.title,
                        &context,
                        &hints,
                        &self.paint,
                    );
                    let body = self.draw_preview(frame, body);
                    let mut items = Vec::new();
                    if self.mode == ManagementMode::Status
                        && super::text_width::terminal_width_observation() == Some(2)
                    {
                        let message = super::i18n::Message::TerminalWidth { observed: Some(2) }
                            .render(language);
                        items.push(popup::Item::new(
                            Line::from(message).style(Style::default().fg(theme::EMPHASIS)),
                        ));
                    }
                    let mut selected_row = 0;
                    let mut previous_group = "";
                    for (index, field) in structured.fields.iter().enumerate() {
                        let (group, label) = field
                            .label
                            .split_once(" › ")
                            .unwrap_or(("", field.label.as_str()));
                        if group != previous_group && !group.is_empty() {
                            items.push(popup::Item::new(
                                Line::from(crate::ui::text_width::glyphs::formatted!(
                                    "── {group} ──",
                                    "-- {group} --"
                                ))
                                .style(Style::default().fg(theme::GROUP_HEADING)),
                            ));
                        }
                        previous_group = group;
                        if index == structured.selected {
                            selected_row = items.len();
                        }
                        let value = crate::presentation::sanitize_plain(field.displayed(), 0).0;
                        let label = crate::presentation::sanitize_plain(label, 0)
                            .0
                            .replace('\n', " ");
                        let mut value_lines = value.lines();
                        let first_value = value_lines.next().unwrap_or("").to_owned();
                        let mut lines = vec![Line::from(vec![
                            Span::styled(
                                format!("{label}: "),
                                Style::default().fg(theme::EMPHASIS),
                            ),
                            Span::raw(first_value),
                        ])];
                        lines =
                            presentation::hanging_wrap(lines, body.width.saturating_sub(3).max(1));
                        items.push(popup::Item::new(lines));
                    }
                    if items.is_empty() {
                        items.push(popup::Item::new(local!(
                            self.language,
                            "表示する項目はありません",
                            "No fields to display"
                        )));
                    }
                    popup::list(frame, body, items, selected_row);
                }
            } else {
                let context = match self.mode {
                    ManagementMode::Index => match self.language {
                        UiLanguage::En => format!("Target {}{sep}{status}", self.index_label()),
                        UiLanguage::Ja => format!("対象 {}{sep}{status}", self.index_label()),
                    },
                    _ => status.to_owned(),
                };
                let body =
                    popup::panel_recorded(frame, area, &view.title, &context, &hints, &self.paint);
                let body = self.draw_preview(frame, body);
                reader_scroll = Some(self.reader_cache.borrow_mut().render(
                    frame,
                    body,
                    self.reader_revision,
                    view.scroll,
                    || {
                        crate::presentation::sanitize_plain(&view.text, 0)
                            .0
                            .lines()
                            .map(|line| Line::from(line.to_owned()))
                            .collect::<Vec<_>>()
                    },
                ));
            }
            if let Some(scroll) = reader_scroll {
                self.view_mut().unwrap().scroll = scroll;
            }
        }
    }
}
fn mark_truncated_preview(lines: &mut Vec<Line<'static>>, height: u16, language: UiLanguage) {
    if lines.len() > usize::from(height) {
        lines.truncate(usize::from(height));
        if let Some(last) = lines.last_mut() {
            *last = Line::from(crate::ui::text_width::glyphs::localized!(
                language,
                "… 省略（取得時刻を含む場合あり）",
                ". 省略（取得時刻を含む場合あり）",
                "… Omitted (may include capture time)",
                ". Omitted (may include capture time)"
            ));
        }
    }
}

pub(super) fn explicit_index_targets(args: &IndexArgs) -> bool {
    args.scope.is_some() || !args.thread_ids.is_empty() || !args.source_ids.is_empty()
}

#[cfg(test)]
mod preview_notice_tests {
    use super::*;

    #[test]
    fn single_management_field_count_is_bilingual_at_standard_widths() {
        fn configure(ui: &mut UiState) {
            ui.management.open_structured(
                "Synthetic".into(),
                1,
                vec![PresentationField {
                    key: "/state".into(),
                    label: "State".into(),
                    value: "ready".into(),
                    ..Default::default()
                }],
                false,
                false,
            );
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["1 / 1 field", "State"],
                UiLanguage::Ja => &["1 / 1項目", "State"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn index_loading_preview_is_bilingual_at_standard_widths() {
        fn configure(ui: &mut UiState) {
            let args = IndexArgs {
                scope: Some(IndexScope::All),
                ..Default::default()
            };
            ui.management
                .open_structured("Index fixture".into(), 1, vec![], false, false);
            ui.management.mode = ManagementMode::Index;
            ui.management.scope = Some(args.clone());
            ui.management.preview = IndexPreviewState::Loading(args);
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["0 / 0 fields", "Cost preview", "API not called"],
                UiLanguage::Ja => &["0 / 0項目", "費用preview", "API未送信"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn index_confirmation_preserves_preview_and_cost_warning() {
        let mut popup = ManagementPopup::default();
        popup.set_language(UiLanguage::En);
        let args = IndexArgs {
            scope: Some(IndexScope::All),
            ..Default::default()
        };
        popup.mode = ManagementMode::Index;
        popup.scope = Some(args.clone());
        popup.preview = IndexPreviewState::Ready(args, "PREVIEW-OPAQUE-0123".into());
        popup.ready_preview_attempt = Some(9);
        let key = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE);
        let Outcome::Confirm {
            command,
            detail,
            attempt,
        } = popup.key(key, 10, None)
        else {
            panic!("the ready preview must show confirmation before executing")
        };
        assert!(matches!(*command, Command::IndexBackfill(_)));
        assert_eq!(attempt, 9);
        assert!(detail.contains("PREVIEW-OPAQUE-0123"));
        assert!(detail.contains("external embedding API") && detail.contains("costs"));
        assert!(detail.contains("monetary cost is not estimated"));
    }

    #[test]
    fn status_shows_measured_width_mismatch_in_both_languages() {
        use ratatui::{Terminal, backend::TestBackend};
        super::super::text_width::set_terminal_width(Some(2));
        for (language, expected) in [
            (crate::config::UiLanguage::En, "differs from Ratatui"),
            (crate::config::UiLanguage::Ja, "Ratatui既定値"),
        ] {
            let mut popup = ManagementPopup::default();
            popup.open_structured(
                "Status".into(),
                1,
                vec![PresentationField {
                    key: "/state".into(),
                    label: "State".into(),
                    value: "ready".into(),
                    ..Default::default()
                }],
                false,
                true,
            );
            let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
            terminal
                .draw(|frame| popup.draw(frame, frame.area(), "", language))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                text.replace(' ', "").contains(&expected.replace(' ', "")),
                "{text}"
            );
        }
        super::super::text_width::clear_terminal_width();
    }

    #[test]
    fn truncation_marks_omission_even_when_only_one_row_fits() {
        let mut lines = vec![Line::from("title"), Line::from("time")];
        mark_truncated_preview(&mut lines, 1, UiLanguage::Ja);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].to_string().starts_with('.'));
        let mut full = vec![Line::from("time")];
        mark_truncated_preview(&mut full, 1, UiLanguage::Ja);
        assert_eq!(full[0].to_string(), "time");
    }
}

#[cfg(test)]
mod initial_management_screen_tests {
    use super::*;
    use crate::config::UiLanguage;
    use std::path::PathBuf;

    #[test]
    fn initial_index_screen_is_bilingual_and_discloses_api_cost() {
        fn configure(ui: &mut UiState) {
            ui.initialized = true;
            ui.insert("/index", false);
            let actions = ui.submit();
            fn command(action: &UiAction) -> Option<&Command> {
                match action {
                    UiAction::SubmittedCommand { action, .. }
                    | UiAction::ManagementIntent { action, .. } => command(action),
                    UiAction::Manage(command) => Some(command),
                    _ => None,
                }
            }
            assert!(
                actions
                    .iter()
                    .filter_map(command)
                    .any(|command| matches!(command, Command::IndexStatus(_)))
            );
            assert!(!actions.iter().filter_map(command).any(|command| matches!(
                command,
                Command::IndexBackfill(_) | Command::IndexRebuild(_)
            )));
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &[
                    "Index management",
                    "past record",
                    "External API use may incur charges",
                ],
                UiLanguage::Ja => &["Index管理", "停止理由は過去の記録", "外部APIの利用で費用"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn initial_attachment_screen_preserves_raw_fields_in_both_languages() {
        fn configure(ui: &mut UiState) {
            ui.initialized = true;
            ui.active_view_mut().draft.insert_attachment(
                UiAttachment {
                    id: "RAW-ID".into(),
                    path: PathBuf::from("RAW-PATH"),
                    kind: "RAW-KIND".into(),
                    state: "RAW-STATE".into(),
                    position: None,
                },
                0,
            );
            ui.insert("/attachments", false);
            ui.submit();
            let page = ui.management.as_ref().unwrap();
            for value in [
                "RAW-ID",
                "RAW-PATH",
                "RAW-KIND",
                "RAW-STATE",
                "selected/managed/unavailable_reason",
                "/detach ID",
            ] {
                assert!(page.text.contains(value), "{value}: {}", page.text);
            }
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &[
                    "Attachment management",
                    "RAW-ID",
                    "RAW-KIND",
                    "RAW-STATE",
                    "RAW-PATH",
                ],
                UiLanguage::Ja => &["添付管理", "RAW-ID", "RAW-KIND", "RAW-STATE", "RAW-PATH"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn initial_received_screen_is_bilingual() {
        fn configure(ui: &mut UiState) {
            ui.initialized = true;
            ui.select_thread("thread-fixture".into());
            ui.insert("/received", false);
            let actions = ui.submit();
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action, UiAction::LoadReceived { page: 1, .. }))
            );
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Received records", "Loading", "Esc"],
                UiLanguage::Ja => &["受信記録", "読込み中", "Esc"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn initial_client_operation_screen_keeps_accepted_work_warning() {
        fn configure(ui: &mut UiState) {
            let operation = crate::client_operation::ClientOperation::Config(
                crate::config_setup::ConfigCommand::Init,
            );
            let actions = ui.execute_client_operation(operation);
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action, UiAction::ClientOperation(_)))
            );
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En
                    if super::super::text_width::terminal_width_observation() == Some(1) =>
                {
                    &["Config · Create template", "Accepted work continues"]
                }
                UiLanguage::En => &["Config | Create template", "Accepted work continues"],
                UiLanguage::Ja
                    if super::super::text_width::terminal_width_observation() == Some(1) =>
                {
                    &["設定 · テンプレート作成", "閉じても受付済みの処理は続行"]
                }
                UiLanguage::Ja => &["設定 | テンプレート作成", "閉じても受付済みの処理は続行"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }
}
